//! Turns decoded records into the DXF document structure: tables, blocks, entities and the
//! header variables the drawing pipeline reads (spec: docs/specs/DWG.md §9).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use super::annot::{self, ImageDef};
use super::container::Sections;
use super::header::{self, codepage_index, codepage_name};
use super::objects::{self as o, ECom, Edge, Kind, Lt, Owned, Path, TextD, P3};
use super::{err, DwgReport, Version, R};
use crate::dxf::{
    Arc, AttDef, Attrib, Block, Circle, Common, DxfDocument, Entity, HeaderVar, Insert, Layer,
    Line, LineType, LwPolyline, LwVertex, MText, Pair, RawSection, Text, TextStyle,
};

/// Upper bound on inserts generated from one rectangular array insert.
const MAX_GRID: usize = 10_000;

const LW: [i32; 24] = [
    0, 5, 9, 13, 15, 18, 20, 25, 30, 35, 40, 50, 53, 60, 70, 80, 90, 100, 106, 120, 140, 158, 200,
    211,
];

/// Class names that are graphic entities (only used to count what is skipped).
const ENTITY_CLASSES: &[&str] = &[
    "IMAGE",
    "WIPEOUT",
    "MULTILEADER",
    "ARC_DIMENSION",
    "LARGE_RADIAL_DIMENSION",
    "MESH",
    "HELIX",
    "LIGHT",
    "CAMERA",
    "PDFUNDERLAY",
    "DWFUNDERLAY",
    "DGNUNDERLAY",
    "SECTIONOBJECT",
    "PLANESURFACE",
    "EXTRUDEDSURFACE",
    "LOFTEDSURFACE",
    "REVOLVEDSURFACE",
    "SWEPTSURFACE",
    "NURBSURFACE",
    "GEOPOSITIONMARKER",
    "POINTCLOUD",
    "POINTCLOUDEX",
    "ACAD_TABLE",
    "ACAD_PROXY_ENTITY",
    "ACIDBLOCKREFERENCE",
];

struct Ent {
    h: u64,
    com: ECom,
    kind: Kind,
}

#[derive(Default)]
struct Tables {
    layers: HashMap<u64, o::LayerRec>,
    ltypes: HashMap<u64, o::LtypeRec>,
    styles: HashMap<u64, o::StyleRec>,
    blocks: Vec<(u64, o::BlockRec)>,
    order: (Vec<u64>, Vec<u64>, Vec<u64>),
    /// Dimension style names (LEADER references).
    dimstyles: HashMap<u64, String>,
    /// Image definitions (IMAGE references), emitted into OBJECTS.
    imagedefs: HashMap<u64, ImageDef>,
    /// Largest handle in the file; synthetic OBJECTS records are numbered above it.
    max_handle: u64,
}

pub(crate) fn document(secs: &Sections, ver: Version, rep: &mut DwgReport) -> R<DxfDocument> {
    let cp = codepage_index(secs.codepage);
    let hdr = header::header(&secs.header, ver, secs.codepage, rep);
    let class_list = header::classes(&secs.classes, ver, secs.codepage, rep);
    let entity_classes: HashSet<u16> = class_list
        .iter()
        .filter(|c| c.entity)
        .map(|c| c.num)
        .collect();
    let classes: HashMap<u16, String> = class_list.into_iter().map(|c| (c.num, c.dxf)).collect();
    let map = o::handle_map(&secs.handles, rep);
    rep.objects = map.len();
    let objs: &[u8] = &secs.objects;
    let mut t = Tables::default();
    let mut ents: Vec<Ent> = vec![];
    let mut failed_kinds: HashMap<String, (usize, String)> = HashMap::new();
    let mut types: HashMap<u64, u16> = HashMap::new();
    let mut dicts: Vec<Vec<(String, u64)>> = vec![];
    // Best-effort kinds that did not decode: count and first reason.
    let mut soft: HashMap<String, (usize, String)> = HashMap::new();
    t.max_handle = map.iter().map(|m| m.0).max().unwrap_or(0);
    for &(handle, off) in &map {
        let f = match o::frame(objs, off, ver) {
            Ok(f) => f,
            Err(_) => {
                rep.failed += 1;
                continue;
            }
        };
        if !f.crc_ok {
            rep.crc_mismatches += 1;
        }
        let class = classes.get(&f.ty).map(String::as_str);
        let own = if f.handle != 0 { f.handle } else { handle };
        types.insert(own, f.ty);
        let res: R<()> = (|| {
            match f.ty {
                // Optional metadata (layout names): a record that does not decode is ignored.
                42 => {
                    let d = o::open_object(&f, ver, cp)
                        .and_then(|mut tri| o::dictionary(&mut tri, ver, own));
                    if let Ok(d) = d {
                        dicts.push(d);
                    }
                }
                49 | 51 | 53 | 57 => {
                    let mut tri = o::open_object(&f, ver, cp)?;
                    match f.ty {
                        49 => t.blocks.push((own, o::block_rec(&mut tri, ver, own)?)),
                        51 => {
                            t.layers.insert(own, o::layer(&mut tri, ver, own)?);
                            t.order.0.push(own);
                        }
                        53 => {
                            t.styles.insert(own, o::style(&mut tri, ver)?);
                            t.order.2.push(own);
                        }
                        _ => {
                            t.ltypes.insert(own, o::ltype(&mut tri, ver)?);
                            t.order.1.push(own);
                        }
                    }
                }
                69 => {
                    let n = o::open_object(&f, ver, cp)
                        .and_then(|mut tri| o::table_name(&mut tri, ver));
                    if let Ok(n) = n {
                        t.dimstyles.insert(own, n);
                    }
                }
                _ if class == Some("IMAGEDEF") => {
                    let d =
                        o::open_object(&f, ver, cp).and_then(|mut tri| annot::imagedef(&mut tri));
                    if let Ok(d) = d {
                        t.imagedefs.insert(own, d);
                    }
                }
                ty if o::is_entity_type(ty)
                    || matches!(class, Some("LWPOLYLINE") | Some("HATCH"))
                    || class.is_some_and(|c| ENTITY_CLASSES.contains(&c)) =>
                {
                    let name = kind_name(ty, class);
                    let (mut tri, com) = o::open_entity(&f, ver, cp)?;
                    match o::entity(&mut tri, ver, ty, class, own) {
                        Ok(Some(kind)) => ents.push(Ent { h: own, com, kind }),
                        Ok(None) => rep.skip(&name),
                        Err(e) if o::best_effort(ty, class) => {
                            rep.skip(&name);
                            let k = soft.entry(name).or_default();
                            k.0 += 1;
                            if k.1.is_empty() {
                                k.1 = e.0;
                            }
                        }
                        Err(e) => return Err(e),
                    }
                }
                ty if entity_classes.contains(&ty) => rep.skip(&kind_name(ty, class)),
                _ => {}
            }
            Ok(())
        })();
        if let Err(e) = res {
            rep.failed += 1;
            let k = failed_kinds.entry(kind_name(f.ty, class)).or_default();
            k.0 += 1;
            if k.1.is_empty() {
                k.1 = e.0;
            }
        }
    }
    let mut fk: Vec<_> = failed_kinds.into_iter().collect();
    fk.sort();
    for (k, (n, first)) in fk {
        rep.warn(format!(
            "{n} {k} record(s) could not be decoded (first: {first})"
        ));
    }
    let mut sk: Vec<_> = soft.into_iter().collect();
    sk.sort();
    for (k, (n, first)) in sk {
        rep.warn(format!("{n} {k} record(s) not converted (first: {first})"));
    }
    let ole = ents
        .iter()
        .filter(|e| matches!(e.kind, Kind::Ole { .. }))
        .count();
    if ole > 0 {
        rep.warn(format!(
            "{ole} OLE2FRAME object(s): frame kept, embedded document not carried"
        ));
    }
    if let Some(&n) = rep.skipped.get("OLE2FRAME") {
        rep.warn(format!(
            "{n} OLE2FRAME object(s) (embedded documents) skipped"
        ));
    }
    rep.layouts = layout_names(&dicts, &types, &classes);
    assemble(&t, ents, ver, &hdr, secs.codepage, rep)
}

/// Layout names from the layout dictionary: the dictionary whose entries resolve to
/// LAYOUT objects and include "Model". Stored order; "Model" first.
fn layout_names(
    dicts: &[Vec<(String, u64)>],
    types: &HashMap<u64, u16>,
    classes: &HashMap<u16, String>,
) -> Vec<String> {
    let is_layout = |h: &u64| {
        types
            .get(h)
            .and_then(|t| classes.get(t))
            .is_some_and(|c| c == "LAYOUT")
    };
    for d in dicts {
        let names: Vec<&str> = d
            .iter()
            .filter(|(_, h)| is_layout(h))
            .map(|(n, _)| n.as_str())
            .collect();
        if names.iter().any(|n| n.eq_ignore_ascii_case("Model")) {
            let mut out: Vec<String> = names.into_iter().map(str::to_string).collect();
            out.sort_by_key(|n| !n.eq_ignore_ascii_case("Model"));
            return out;
        }
    }
    vec![]
}

fn kind_name(ty: u16, class: Option<&str>) -> String {
    if let Some(c) = class {
        return c.to_string();
    }
    const N: [&str; 42] = [
        "UNUSED",
        "TEXT",
        "ATTRIB",
        "ATTDEF",
        "BLOCK",
        "ENDBLK",
        "SEQEND",
        "INSERT",
        "MINSERT",
        "OBJECT9",
        "VERTEX",
        "VERTEX",
        "VERTEX",
        "VERTEX",
        "VERTEX",
        "POLYLINE",
        "POLYLINE",
        "ARC",
        "CIRCLE",
        "LINE",
        "DIMENSION",
        "DIMENSION",
        "DIMENSION",
        "DIMENSION",
        "DIMENSION",
        "DIMENSION",
        "DIMENSION",
        "POINT",
        "3DFACE",
        "POLYLINE",
        "POLYLINE",
        "SOLID",
        "TRACE",
        "SHAPE",
        "VIEWPORT",
        "ELLIPSE",
        "SPLINE",
        "REGION",
        "3DSOLID",
        "BODY",
        "RAY",
        "XLINE",
    ];
    match ty {
        44 => "MTEXT".into(),
        77 => "LWPOLYLINE".into(),
        78 => "HATCH".into(),
        9 | 33 | 37..=39 | 43 | 45..=47 | 74 => o::type_name(ty).into(),
        t if (t as usize) < N.len() => N[t as usize].into(),
        t => format!("TYPE{t}"),
    }
}

struct Ctx<'a> {
    t: &'a Tables,
    ents: &'a HashMap<u64, usize>,
    all: &'a [Ent],
    children: &'a HashMap<u64, Vec<u64>>,
    blocks: HashMap<u64, &'a str>,
    /// INSERTs whose block record is unknown (reported as skipped).
    unresolved: Cell<usize>,
    /// ACAD_TABLE entities without a usable block (reported as skipped).
    tables_no_block: Cell<usize>,
}

/// Walks an R13–R2000 sibling chain from `first` through the next links up to `last`,
/// keeping only entities accepted by `member`; each handle is visited once (cycle guard).
fn walk_chain(
    index: &HashMap<u64, usize>,
    all: &[Ent],
    first: u64,
    last: u64,
    member: impl Fn(usize) -> bool,
) -> Vec<usize> {
    let mut out = vec![];
    let mut seen = HashSet::new();
    let mut cur = first;
    while let Some(&i) = index.get(&cur) {
        if !seen.insert(cur) || !member(i) {
            break;
        }
        out.push(i);
        if cur == last {
            break;
        }
        cur = all[i].com.next;
    }
    out
}

/// `members` in chain order; members the chain does not reach keep their stored order after it.
fn chain_order(members: &[usize], chain: &[usize]) -> Vec<usize> {
    let on: HashSet<usize> = chain.iter().copied().collect();
    let mut out = chain.to_vec();
    out.extend(members.iter().copied().filter(|i| !on.contains(i)));
    out
}

fn num(v: f64) -> String {
    format!("{v}")
}

fn p(code: i32, v: impl Into<String>) -> Pair {
    Pair::new(code, v)
}

fn pt3(out: &mut Vec<Pair>, base: i32, v: P3) {
    out.push(p(base, num(v[0])));
    out.push(p(base + 10, num(v[1])));
    out.push(p(base + 20, num(v[2])));
}

fn pt2(out: &mut Vec<Pair>, base: i32, v: [f64; 2]) {
    out.push(p(base, num(v[0])));
    out.push(p(base + 10, num(v[1])));
}

fn ext_pairs(out: &mut Vec<Pair>, ext: P3, th: f64) {
    if th != 0.0 {
        out.push(p(39, num(th)));
    }
    if ext != [0.0, 0.0, 1.0] && ext != [0.0; 3] {
        pt3(out, 210, ext);
    }
}

impl Ctx<'_> {
    fn common(&self, c: &ECom, paper: bool) -> Common {
        let layer = self
            .t
            .layers
            .get(&c.layer)
            .map(|l| l.name.clone())
            .unwrap_or_else(|| "0".into());
        let linetype = match c.lt {
            Lt::ByLayer => None,
            Lt::ByBlock => Some("ByBlock".to_string()),
            Lt::Continuous => Some("Continuous".to_string()),
            Lt::Handle(h) => self
                .t
                .ltypes
                .get(&h)
                .map(|l| l.name.clone())
                .filter(|n| !n.eq_ignore_ascii_case("ByLayer")),
        };
        let lineweight = match c.lw_index {
            Some(i) if (i as usize) < LW.len() => Some(LW[i as usize]),
            Some(30) => Some(-2),
            Some(31) => Some(-3),
            _ => None,
        };
        let mut extra = vec![];
        if paper {
            extra.push(p(67, "1"));
        }
        if c.ltscale != 1.0 && c.ltscale.is_finite() && c.ltscale != 0.0 {
            extra.push(p(48, num(c.ltscale)));
        }
        if c.invisible {
            extra.push(p(60, "1"));
        }
        Common {
            layer,
            linetype,
            color: (c.color != 256).then_some(c.color),
            true_color: c.true_color,
            lineweight,
            extra,
        }
    }

    fn style(&self, h: u64) -> Option<String> {
        self.t.styles.get(&h).map(|s| s.name.clone())
    }

    /// Children of a POLYLINE / INSERT in stored order (list, or R13–R2000 link chain).
    fn kids(&self, h: u64, owned: &Owned) -> Vec<&Ent> {
        let stored: &[u64] = self.children.get(&h).map(Vec::as_slice).unwrap_or(&[]);
        let list: &[u64] = match owned {
            Owned::List(l) if !l.is_empty() => l.as_slice(),
            Owned::Chain(first, last) => {
                let members: Vec<usize> = stored
                    .iter()
                    .filter_map(|k| self.ents.get(k).copied())
                    .collect();
                let set: HashSet<usize> = members.iter().copied().collect();
                let chain = walk_chain(self.ents, self.all, *first, *last, |i| set.contains(&i));
                return chain_order(&members, &chain)
                    .into_iter()
                    .map(|i| &self.all[i])
                    .collect();
            }
            _ => stored,
        };
        list.iter()
            .filter_map(|k| self.ents.get(k).map(|&i| &self.all[i]))
            .collect()
    }

    /// Verbatim record with the entity header pairs, as the DXF reader keeps unknown kinds.
    fn raw(&self, name: &str, c: &Common, sub: &[&str], body: Vec<Pair>) -> Entity {
        let mut v = vec![p(0, name), p(100, "AcDbEntity")];
        v.extend(c.extra.iter().filter(|x| x.code == 67).cloned());
        v.push(p(8, c.layer.clone()));
        if let Some(l) = &c.linetype {
            v.push(p(6, l.clone()));
        }
        if let Some(col) = c.color {
            v.push(p(62, col.to_string()));
        }
        if let Some(col) = c.true_color {
            v.push(p(420, col.to_string()));
        }
        if let Some(w) = c.lineweight {
            v.push(p(370, w.to_string()));
        }
        v.extend(c.extra.iter().filter(|x| x.code != 67).cloned());
        for s in sub {
            v.push(p(100, *s));
        }
        v.extend(body);
        Entity::Unknown(v)
    }

    fn text_extra(c: &mut Common, d: &TextD) {
        ext_pairs(&mut c.extra, d.ext, d.thickness);
        if d.width != 1.0 {
            c.extra.push(p(41, num(d.width)));
        }
        if d.oblique != 0.0 {
            c.extra.push(p(51, num(d.oblique.to_degrees())));
        }
        if d.generation != 0 {
            c.extra.push(p(71, d.generation.to_string()));
        }
        if d.halign != 0 {
            c.extra.push(p(72, d.halign.to_string()));
        }
        if let Some(a) = d.align {
            if d.halign != 0 || d.valign != 0 {
                pt3(&mut c.extra, 11, [a[0], a[1], d.ins[2]]);
            }
        }
        if d.valign != 0 {
            c.extra.push(p(73, d.valign.to_string()));
        }
    }

    fn attrib(&self, e: &Ent) -> Option<Attrib> {
        let Kind::Attrib { t, tag, flags } = &e.kind else {
            return None;
        };
        let mut common = self.common(&e.com, false);
        Self::text_extra(&mut common, t);
        if t.rotation != 0.0 {
            common.extra.push(p(50, num(t.rotation.to_degrees())));
        }
        Some(Attrib {
            common,
            tag: tag.clone(),
            value: t.text.clone(),
            pos: t.ins,
            height: t.height,
            flags: *flags as i32,
        })
    }

    fn convert(&self, e: &Ent, paper: bool, out: &mut Vec<Entity>) {
        let mut c = self.common(&e.com, paper);
        match &e.kind {
            Kind::Line { a, b, th, ext } => {
                ext_pairs(&mut c.extra, *ext, *th);
                out.push(Entity::Line(Line {
                    common: c,
                    start: *a,
                    end: *b,
                }));
            }
            Kind::Circle { c: ctr, r, th, ext } => {
                ext_pairs(&mut c.extra, *ext, *th);
                out.push(Entity::Circle(Circle {
                    common: c,
                    center: *ctr,
                    radius: *r,
                }));
            }
            Kind::Arc {
                c: ctr,
                r,
                th,
                ext,
                a0,
                a1,
            } => {
                ext_pairs(&mut c.extra, *ext, *th);
                out.push(Entity::Arc(Arc {
                    common: c,
                    center: *ctr,
                    radius: *r,
                    start_angle: a0.to_degrees(),
                    end_angle: a1.to_degrees(),
                }));
            }
            Kind::Text(d) => {
                Self::text_extra(&mut c, d);
                out.push(Entity::Text(Text {
                    common: c,
                    pos: d.ins,
                    height: d.height,
                    text: d.text.clone(),
                    rotation: d.rotation.to_degrees(),
                    style: self.style(d.style),
                }));
            }
            Kind::AttDef {
                t: d,
                tag,
                prompt,
                flags,
            } => {
                Self::text_extra(&mut c, d);
                if d.rotation != 0.0 {
                    c.extra.push(p(50, num(d.rotation.to_degrees())));
                }
                out.push(Entity::AttDef(AttDef {
                    common: c,
                    tag: tag.clone(),
                    prompt: prompt.clone(),
                    default: d.text.clone(),
                    pos: d.ins,
                    height: d.height,
                    flags: *flags as i32,
                }));
            }
            Kind::MText {
                ins,
                ext,
                xdir,
                width,
                height,
                attach,
                text,
                style,
            } => {
                ext_pairs(&mut c.extra, *ext, 0.0);
                out.push(Entity::MText(MText {
                    common: c,
                    pos: *ins,
                    height: *height,
                    width: *width,
                    text: text.clone(),
                    rotation: xdir[1].atan2(xdir[0]).to_degrees(),
                    style: self.style(*style),
                    attach: Some(*attach as i32),
                }));
            }
            Kind::Table {
                ins,
                rot,
                ext,
                block,
            } => {
                let Some(name) = self.blocks.get(block) else {
                    self.tables_no_block.set(self.tables_no_block.get() + 1);
                    return;
                };
                ext_pairs(&mut c.extra, *ext, 0.0);
                out.push(Entity::Insert(Insert {
                    common: c,
                    block: name.to_string(),
                    pos: *ins,
                    scale: [1.0; 3],
                    rotation: rot.to_degrees(),
                    attribs: vec![],
                }));
            }
            Kind::Insert {
                ins,
                scale,
                rot,
                ext,
                block,
                attribs,
                grid,
            } => {
                let Some(name) = self.blocks.get(block) else {
                    self.unresolved.set(self.unresolved.get() + 1);
                    return;
                };
                ext_pairs(&mut c.extra, *ext, 0.0);
                let atts: Vec<Attrib> = self
                    .kids(e.h, attribs)
                    .into_iter()
                    .filter_map(|k| self.attrib(k))
                    .collect();
                let (cols, rows, dc, dr) = grid.unwrap_or((1, 1, 0.0, 0.0));
                let (cols, rows) = (cols.max(1) as usize, rows.max(1) as usize);
                let (s, co) = rot.sin_cos();
                let mut first = true;
                'grid: for j in 0..rows {
                    for i in 0..cols {
                        if j * cols + i >= MAX_GRID {
                            break 'grid;
                        }
                        let (dx, dy) = (i as f64 * dc, j as f64 * dr);
                        out.push(Entity::Insert(Insert {
                            common: c.clone(),
                            block: name.to_string(),
                            pos: [ins[0] + dx * co - dy * s, ins[1] + dx * s + dy * co, ins[2]],
                            scale: *scale,
                            rotation: rot.to_degrees(),
                            attribs: if first { atts.clone() } else { vec![] },
                        }));
                        first = false;
                    }
                }
            }
            Kind::LwPoly {
                flags,
                const_w,
                elev,
                th,
                ext,
                pts,
                bulges,
                widths,
            } => {
                let mut f = 0;
                if flags & 512 != 0 {
                    f |= 1;
                }
                if flags & 256 != 0 {
                    f |= 128;
                }
                if *elev != 0.0 {
                    c.extra.push(p(38, num(*elev)));
                }
                if *const_w != 0.0 {
                    c.extra.push(p(43, num(*const_w)));
                }
                ext_pairs(&mut c.extra, *ext, *th);
                let vertices = pts
                    .iter()
                    .enumerate()
                    .map(|(i, q)| {
                        let (w0, w1) = widths.get(i).copied().unwrap_or((*const_w, *const_w));
                        LwVertex {
                            x: q[0],
                            y: q[1],
                            bulge: bulges.get(i).copied().unwrap_or(0.0),
                            start_width: w0,
                            end_width: w1,
                        }
                    })
                    .collect();
                out.push(Entity::LwPolyline(LwPolyline {
                    common: c,
                    flags: f,
                    vertices,
                }));
            }
            Kind::Poly2d {
                flags,
                w0,
                w1,
                th,
                elev,
                ext,
                owned,
            } => {
                if *elev != 0.0 {
                    c.extra.push(p(38, num(*elev)));
                }
                ext_pairs(&mut c.extra, *ext, *th);
                let vertices = self
                    .kids(e.h, owned)
                    .into_iter()
                    .filter_map(|k| match &k.kind {
                        Kind::Vertex {
                            flags,
                            p: q,
                            w0: a,
                            w1: b,
                            bulge,
                        } if flags & 16 == 0 => {
                            let (sw, ew) = if *a == 0.0 && *b == 0.0 {
                                (*w0, *w1)
                            } else {
                                (*a, *b)
                            };
                            Some(LwVertex {
                                x: q[0],
                                y: q[1],
                                bulge: *bulge,
                                start_width: sw,
                                end_width: ew,
                            })
                        }
                        _ => None,
                    })
                    .collect();
                out.push(Entity::LwPolyline(LwPolyline {
                    common: c,
                    flags: (*flags as i32) & (1 | 128),
                    vertices,
                }));
            }
            Kind::PolyN { flags, m, n, owned } => {
                let sub = match flags & (8 | 16 | 64) {
                    8 => "AcDb3dPolyline",
                    64 => "AcDbPolyFaceMesh",
                    _ => "AcDbPolygonMesh",
                };
                let mut body = vec![p(66, "1")];
                pt3(&mut body, 10, [0.0; 3]);
                body.push(p(70, flags.to_string()));
                if *m != 0 || *n != 0 {
                    body.push(p(71, m.to_string()));
                    body.push(p(72, n.to_string()));
                }
                out.push(self.raw("POLYLINE", &c, &[sub], body));
                let vsub = match flags & (8 | 16 | 64) {
                    8 => ("AcDb3dPolylineVertex", 32),
                    64 => ("AcDbPolyFaceMeshVertex", 192),
                    _ => ("AcDbPolygonMeshVertex", 64),
                };
                for k in self.kids(e.h, owned) {
                    let kc = self.common(&k.com, paper);
                    match &k.kind {
                        Kind::Vertex {
                            flags: vf, p: q, ..
                        } => {
                            let mut b = vec![];
                            pt3(&mut b, 10, *q);
                            b.push(p(70, ((*vf as i32) | vsub.1).to_string()));
                            out.push(self.raw("VERTEX", &kc, &["AcDbVertex", vsub.0], b));
                        }
                        Kind::FaceRec(ix) => {
                            let mut b = vec![];
                            pt3(&mut b, 10, [0.0; 3]);
                            b.push(p(70, "128"));
                            for (i, v) in ix.iter().enumerate() {
                                if *v != 0 {
                                    b.push(p(71 + i as i32, v.to_string()));
                                }
                            }
                            out.push(self.raw("VERTEX", &kc, &["AcDbFaceRecord"], b));
                        }
                        _ => {}
                    }
                }
                out.push(self.raw("SEQEND", &c, &[], vec![]));
            }
            Kind::Point { p: q, th, ext } => {
                let mut b = vec![];
                pt3(&mut b, 10, *q);
                ext_pairs(&mut b, *ext, *th);
                out.push(self.raw("POINT", &c, &["AcDbPoint"], b));
            }
            Kind::Ellipse {
                c: ctr,
                major,
                ext,
                ratio,
                a0,
                a1,
            } => {
                let mut b = vec![];
                pt3(&mut b, 10, *ctr);
                pt3(&mut b, 11, *major);
                ext_pairs(&mut b, *ext, 0.0);
                b.push(p(40, num(*ratio)));
                b.push(p(41, num(*a0)));
                b.push(p(42, num(*a1)));
                out.push(self.raw("ELLIPSE", &c, &["AcDbEllipse"], b));
            }
            Kind::Spline {
                flags,
                degree,
                knots,
                ctrl,
                weights,
                fit,
                tangents,
            } => {
                let mut f = *flags;
                if !weights.is_empty() {
                    f |= 4;
                }
                let mut b = vec![
                    p(70, f.to_string()),
                    p(71, degree.to_string()),
                    p(72, knots.len().to_string()),
                    p(73, ctrl.len().to_string()),
                    p(74, fit.len().to_string()),
                ];
                if let Some((t0, t1)) = tangents {
                    pt3(&mut b, 12, *t0);
                    pt3(&mut b, 13, *t1);
                }
                b.extend(knots.iter().map(|k| p(40, num(*k))));
                b.extend(weights.iter().map(|w| p(41, num(*w))));
                for q in ctrl {
                    pt3(&mut b, 10, *q);
                }
                for q in fit {
                    pt3(&mut b, 11, *q);
                }
                out.push(self.raw("SPLINE", &c, &["AcDbSpline"], b));
            }
            Kind::Solid {
                trace,
                pts,
                th,
                ext,
            } => {
                let mut b = vec![];
                for (i, q) in pts.iter().enumerate() {
                    pt3(&mut b, 10 + i as i32, *q);
                }
                ext_pairs(&mut b, *ext, *th);
                let name = if *trace { "TRACE" } else { "SOLID" };
                out.push(self.raw(name, &c, &["AcDbTrace"], b));
            }
            Kind::Face3d { pts, flags } => {
                let mut b = vec![];
                for (i, q) in pts.iter().enumerate() {
                    pt3(&mut b, 10 + i as i32, *q);
                }
                if *flags != 0 {
                    b.push(p(70, flags.to_string()));
                }
                out.push(self.raw("3DFACE", &c, &["AcDbFace"], b));
            }
            Kind::Viewport { c: ctr, w, h } => {
                let mut b = vec![];
                pt3(&mut b, 10, *ctr);
                b.push(p(40, num(*w)));
                b.push(p(41, num(*h)));
                out.push(self.raw("VIEWPORT", &c, &["AcDbViewport"], b));
            }
            Kind::Ray { xline, p: q, v } => {
                let mut b = vec![];
                pt3(&mut b, 10, *q);
                pt3(&mut b, 11, *v);
                let (name, sub) = if *xline {
                    ("XLINE", "AcDbXline")
                } else {
                    ("RAY", "AcDbRay")
                };
                out.push(self.raw(name, &c, &[sub], b));
            }
            Kind::Dim(d) => {
                let mut b = vec![];
                if let Some(n) = self.blocks.get(&d.block) {
                    b.push(p(2, *n));
                }
                if let Some(s) = self.style(d.style) {
                    b.push(p(3, s));
                }
                pt3(&mut b, 10, d.p10);
                pt3(&mut b, 11, [d.text_mid[0], d.text_mid[1], d.elevation]);
                let mut kind = d.kind | 32;
                if d.ordinate_x {
                    kind |= 64;
                }
                b.push(p(70, kind.to_string()));
                b.push(p(71, d.attach.to_string()));
                if !d.text.is_empty() {
                    b.push(p(1, d.text.clone()));
                }
                if d.text_rot != 0.0 {
                    b.push(p(53, num(d.text_rot)));
                }
                if d.measure != 0.0 {
                    b.push(p(42, num(d.measure)));
                }
                ext_pairs(&mut b, d.ext, 0.0);
                let subs: &[&str] = match d.kind {
                    0 => &["AcDbAlignedDimension", "AcDbRotatedDimension"],
                    1 => &["AcDbAlignedDimension"],
                    2 => &["AcDb2LineAngularDimension"],
                    3 => &["AcDbDiametricDimension"],
                    4 => &["AcDbRadialDimension"],
                    5 => &["AcDb3PointAngularDimension"],
                    _ => &["AcDbOrdinateDimension"],
                };
                b.push(p(100, subs[0]));
                for (code, v) in [(13, d.p13), (14, d.p14), (15, d.p15)] {
                    if let Some(v) = v {
                        pt3(&mut b, code, v);
                    }
                }
                if let Some(v) = d.p16 {
                    pt3(&mut b, 16, [v[0], v[1], d.elevation]);
                }
                if d.leader != 0.0 {
                    b.push(p(40, num(d.leader)));
                }
                if d.kind <= 1 {
                    b.push(p(50, num(d.rot)));
                    if d.ext_rot != 0.0 {
                        b.push(p(52, num(d.ext_rot)));
                    }
                }
                let mut e = self.raw("DIMENSION", &c, &["AcDbDimension"], b);
                if let (Entity::Unknown(v), Some(s)) = (&mut e, subs.get(1)) {
                    v.push(p(100, *s));
                }
                out.push(e);
            }
            Kind::Hatch {
                elev,
                ext,
                name,
                solid,
                assoc,
                paths,
                style,
                ptype,
                angle,
                scale,
                double,
                lines,
                seeds,
            } => {
                let mut b = vec![];
                pt3(&mut b, 10, [0.0, 0.0, *elev]);
                ext_pairs(&mut b, *ext, 0.0);
                b.push(p(2, name.clone()));
                b.push(p(70, (*solid as i32).to_string()));
                b.push(p(71, (*assoc as i32).to_string()));
                b.push(p(91, paths.len().to_string()));
                for path in paths {
                    hatch_path(&mut b, path);
                    b.push(p(97, "0"));
                }
                b.push(p(75, style.to_string()));
                b.push(p(76, ptype.to_string()));
                if !solid {
                    b.push(p(52, num(*angle)));
                    b.push(p(41, num(*scale)));
                    b.push(p(77, (*double as i32).to_string()));
                    b.push(p(78, lines.len().to_string()));
                    for l in lines {
                        b.push(p(53, num(l.angle)));
                        pt_pair(&mut b, 43, l.base);
                        pt_pair(&mut b, 45, l.offset);
                        b.push(p(79, l.dashes.len().to_string()));
                        for d in &l.dashes {
                            b.push(p(49, num(*d)));
                        }
                    }
                }
                b.push(p(98, seeds.len().to_string()));
                for s in seeds {
                    pt2(&mut b, 10, *s);
                }
                out.push(self.raw("HATCH", &c, &["AcDbHatch"], b));
            }
            Kind::Leader(d) => {
                let mut b = vec![];
                if let Some(s) = self.t.dimstyles.get(&d.style) {
                    b.push(p(3, s.clone()));
                }
                b.push(p(71, (d.arrow as i32).to_string()));
                b.push(p(72, d.path.to_string()));
                b.push(p(73, d.annot.to_string()));
                b.push(p(74, (d.hook as i32).to_string()));
                if d.box_h != 0.0 || d.box_w != 0.0 {
                    b.push(p(40, num(d.box_h)));
                    b.push(p(41, num(d.box_w)));
                }
                b.push(p(76, d.pts.len().to_string()));
                for q in &d.pts {
                    pt3(&mut b, 10, *q);
                }
                ext_pairs(&mut b, d.ext, 0.0);
                pt3(&mut b, 211, d.xdir);
                if d.block_off != [0.0; 3] {
                    pt3(&mut b, 212, d.block_off);
                }
                if d.annot_off != [0.0; 3] {
                    pt3(&mut b, 213, d.annot_off);
                }
                out.push(self.raw("LEADER", &c, &["AcDbLeader"], b));
            }
            Kind::MLeader(d) => {
                let b = mleader_pairs(d);
                out.push(self.raw("MULTILEADER", &c, &["AcDbMLeader"], b));
            }
            Kind::Image { wipeout, d } => {
                let mut b = vec![p(90, "0")];
                pt3(&mut b, 10, d.origin);
                pt3(&mut b, 11, d.u);
                pt3(&mut b, 12, d.v);
                pt2(&mut b, 13, d.size);
                if self.t.imagedefs.contains_key(&d.def) {
                    b.push(p(340, format!("{:X}", d.def)));
                }
                b.push(p(70, d.flags.to_string()));
                b.push(p(280, (d.clip as i32).to_string()));
                b.push(p(281, d.bright.to_string()));
                b.push(p(282, d.contrast.to_string()));
                b.push(p(283, d.fade.to_string()));
                b.push(p(71, d.clip_type.to_string()));
                b.push(p(91, d.clip_pts.len().to_string()));
                for q in &d.clip_pts {
                    pt2(&mut b, 14, *q);
                }
                let (name, subs): (&str, &[&str]) = if *wipeout {
                    ("WIPEOUT", &["AcDbRasterImage", "AcDbWipeout"])
                } else {
                    ("IMAGE", &["AcDbRasterImage"])
                };
                out.push(self.raw(name, &c, subs, b));
            }
            Kind::Acis { name, sat } => {
                let mut b = vec![p(70, "1")];
                if let Some(s) = sat {
                    sat_pairs(&mut b, s);
                }
                out.push(self.raw(name, &c, &["AcDbModelerGeometry"], b));
            }
            Kind::BlockRef {
                ins,
                scale,
                rot,
                ext,
                block,
            } => {
                let Some(name) = self.blocks.get(block) else {
                    self.unresolved.set(self.unresolved.get() + 1);
                    return;
                };
                let mut b = vec![p(2, name.to_string())];
                pt3(&mut b, 10, *ins);
                for (i, s) in scale.iter().enumerate() {
                    if *s != 1.0 {
                        b.push(p(41 + i as i32, num(*s)));
                    }
                }
                if *rot != 0.0 {
                    b.push(p(50, num(rot.to_degrees())));
                }
                ext_pairs(&mut b, *ext, 0.0);
                out.push(self.raw("ACIDBLOCKREFERENCE", &c, &["AcDbBlockReference"], b));
            }
            Kind::Ole { version } => {
                let b = vec![p(70, version.to_string()), p(1, "OLE")];
                out.push(self.raw("OLE2FRAME", &c, &["AcDbOle2Frame"], b));
            }
            Kind::Attrib { .. }
            | Kind::Block
            | Kind::EndBlk
            | Kind::SeqEnd
            | Kind::Vertex { .. }
            | Kind::FaceRec(_) => {}
        }
    }
}

/// MULTILEADER body in DXF order: context data (text, leader roots and lines), then settings.
fn mleader_pairs(d: &annot::MLeaderD) -> Vec<Pair> {
    let mut b = vec![p(270, "2"), p(300, "CONTEXT_DATA{"), p(40, num(d.scale))];
    pt3(&mut b, 10, d.base);
    b.push(p(41, num(d.text_h)));
    b.push(p(140, num(d.arrow)));
    b.push(p(145, num(d.gap)));
    b.push(p(290, (d.text.is_some() as i32).to_string()));
    if let Some(x) = &d.text {
        b.push(p(304, x.text.clone()));
        pt3(&mut b, 11, x.normal);
        pt3(&mut b, 12, x.loc);
        pt3(&mut b, 13, x.dir);
        b.push(p(42, num(x.rot)));
        b.push(p(43, num(x.width)));
        b.push(p(44, num(x.height)));
        b.push(p(171, x.align.to_string()));
    }
    b.push(p(296, (d.block.is_some() as i32).to_string()));
    if let Some((loc, scale, rot)) = &d.block {
        pt3(&mut b, 15, *loc);
        pt3(&mut b, 16, *scale);
        b.push(p(46, num(*rot)));
    }
    for r in &d.roots {
        b.push(p(302, "LEADER{"));
        b.push(p(290, "1"));
        b.push(p(291, "1"));
        pt3(&mut b, 10, r.conn);
        pt3(&mut b, 11, r.dir);
        b.push(p(90, r.index.to_string()));
        b.push(p(40, num(r.landing)));
        for (i, l) in r.lines.iter().enumerate() {
            b.push(p(304, "LEADER_LINE{"));
            for q in l {
                pt3(&mut b, 10, *q);
            }
            b.push(p(91, i.to_string()));
            b.push(p(305, "}"));
        }
        b.push(p(303, "}"));
    }
    b.push(p(301, "}"));
    b.push(p(170, d.leader_type.to_string()));
    b.push(p(291, (d.dogleg as i32).to_string()));
    b.push(p(172, d.content_type.to_string()));
    b
}

/// Modeler text as DXF stores it: one encoded line per code 1, longer lines continued in 3.
fn sat_pairs(b: &mut Vec<Pair>, sat: &str) {
    for line in sat.lines() {
        let enc: Vec<char> = crate::dxf::annot::sat_crypt(line).chars().collect();
        if enc.is_empty() {
            b.push(p(1, ""));
        }
        for (i, part) in enc.chunks(255).enumerate() {
            b.push(p(
                if i == 0 { 1 } else { 3 },
                part.iter().collect::<String>(),
            ));
        }
    }
}

/// Dictionary entry name for an image file: its stem, made unique.
fn image_key(path: &str, i: usize, seen: &mut HashSet<String>) -> String {
    let stem = path
        .rsplit(|c: char| c == '/' || c == '\\')
        .next()
        .and_then(|f| f.split('.').next())
        .filter(|f| !f.is_empty())
        .unwrap_or("IMAGE");
    if seen.insert(stem.to_uppercase()) {
        stem.to_string()
    } else {
        format!("{stem}_{i}")
    }
}

/// OBJECTS for the image definitions: root, group and image dictionaries (numbered above
/// every file handle), then one IMAGEDEF per definition under its own handle.
fn image_objects(t: &Tables) -> RawSection {
    let hex = |h: u64| format!("{h:X}");
    let root = t.max_handle + 1;
    let (group, dict) = (root + 1, root + 2);
    let mut defs: Vec<(&u64, &ImageDef)> = t.imagedefs.iter().collect();
    defs.sort_by_key(|d| *d.0);
    let mut v = vec![
        p(0, "DICTIONARY"),
        p(5, hex(root)),
        p(330, "0"),
        p(100, "AcDbDictionary"),
        p(281, "1"),
        p(3, "ACAD_GROUP"),
        p(350, hex(group)),
        p(3, "ACAD_IMAGE_DICT"),
        p(350, hex(dict)),
    ];
    for h in [group, dict] {
        v.extend([p(0, "DICTIONARY"), p(5, hex(h)), p(330, hex(root))]);
        v.extend([p(100, "AcDbDictionary"), p(281, "1")]);
    }
    let mut seen = HashSet::new();
    for (i, (h, d)) in defs.iter().enumerate() {
        v.push(p(3, image_key(&d.path, i, &mut seen)));
        v.push(p(350, hex(**h)));
    }
    for (h, d) in defs {
        v.extend([p(0, "IMAGEDEF"), p(5, hex(*h)), p(330, hex(dict))]);
        v.extend([
            p(100, "AcDbRasterImageDef"),
            p(90, "0"),
            p(1, d.path.clone()),
        ]);
        pt2(&mut v, 10, d.size);
        pt2(&mut v, 11, d.pixel);
        v.push(p(280, (d.loaded as i32).to_string()));
        v.push(p(281, d.units.to_string()));
    }
    RawSection {
        name: "OBJECTS".into(),
        pairs: v,
    }
}

/// Two consecutive codes (`code`, `code + 1`) for a 2D value, as hatch pattern lines use.
fn pt_pair(out: &mut Vec<Pair>, code: i32, v: [f64; 2]) {
    out.push(p(code, num(v[0])));
    out.push(p(code + 1, num(v[1])));
}

fn hatch_path(b: &mut Vec<Pair>, path: &Path) {
    match path {
        Path::Poly { flags, closed, pts } => {
            b.push(p(92, flags.to_string()));
            let bul = pts.iter().any(|(_, w)| *w != 0.0);
            b.push(p(72, (bul as i32).to_string()));
            b.push(p(73, (*closed as i32).to_string()));
            b.push(p(93, pts.len().to_string()));
            for (q, w) in pts {
                pt2(b, 10, *q);
                if bul {
                    b.push(p(42, num(*w)));
                }
            }
        }
        Path::Edges(flags, edges) => {
            b.push(p(92, flags.to_string()));
            b.push(p(93, edges.len().to_string()));
            for e in edges {
                match e {
                    Edge::Line(a, c) => {
                        b.push(p(72, "1"));
                        pt2(b, 10, *a);
                        pt2(b, 11, *c);
                    }
                    Edge::Arc { c, r, a0, a1, ccw } => {
                        b.push(p(72, "2"));
                        pt2(b, 10, *c);
                        b.push(p(40, num(*r)));
                        b.push(p(50, num(a0.to_degrees())));
                        b.push(p(51, num(a1.to_degrees())));
                        b.push(p(73, (*ccw as i32).to_string()));
                    }
                    Edge::Ellipse {
                        c,
                        major,
                        ratio,
                        a0,
                        a1,
                        ccw,
                    } => {
                        b.push(p(72, "3"));
                        pt2(b, 10, *c);
                        pt2(b, 11, *major);
                        b.push(p(40, num(*ratio)));
                        b.push(p(50, num(a0.to_degrees())));
                        b.push(p(51, num(a1.to_degrees())));
                        b.push(p(73, (*ccw as i32).to_string()));
                    }
                    Edge::Spline {
                        degree,
                        rational,
                        periodic,
                        knots,
                        ctrl,
                    } => {
                        b.push(p(72, "4"));
                        b.push(p(94, degree.to_string()));
                        b.push(p(73, (*rational as i32).to_string()));
                        b.push(p(74, (*periodic as i32).to_string()));
                        b.push(p(95, knots.len().to_string()));
                        b.push(p(96, ctrl.len().to_string()));
                        b.extend(knots.iter().map(|k| p(40, num(*k))));
                        for (q, w) in ctrl {
                            pt2(b, 10, *q);
                            if *rational {
                                b.push(p(42, num(*w)));
                            }
                        }
                    }
                }
            }
        }
    }
}

fn header_vars(ver: Version, hdr: &header::HeaderInfo, cp: u16) -> Vec<HeaderVar> {
    let mut h = vec![];
    let mut var = |name: &str, values: Vec<Pair>| {
        h.push(HeaderVar {
            name: name.into(),
            values,
        })
    };
    var("$ACADVER", vec![p(1, ver.magic())]);
    if let Some(n) = codepage_name(cp) {
        var("$DWGCODEPAGE", vec![p(3, n)]);
    }
    for (name, v) in [
        ("$INSBASE", hdr.insbase),
        ("$EXTMIN", hdr.extmin),
        ("$EXTMAX", hdr.extmax),
    ] {
        if let Some(v) = v {
            let mut ps = vec![];
            pt3(&mut ps, 10, v);
            var(name, ps);
        }
    }
    for (name, v) in [("$LIMMIN", hdr.limmin), ("$LIMMAX", hdr.limmax)] {
        if let Some(v) = v {
            let mut ps = vec![];
            pt2(&mut ps, 10, v);
            var(name, ps);
        }
    }
    if let Some(v) = hdr.ltscale {
        var("$LTSCALE", vec![p(40, num(v))]);
    }
    if let Some(v) = hdr.insunits {
        var("$INSUNITS", vec![p(70, v.to_string())]);
    }
    h
}

fn assemble(
    t: &Tables,
    ents: Vec<Ent>,
    ver: Version,
    hdr: &header::HeaderInfo,
    cp: u16,
    rep: &mut DwgReport,
) -> R<DxfDocument> {
    let mut doc = DxfDocument {
        header: header_vars(ver, hdr, cp),
        ..Default::default()
    };
    let mut model = None;
    let mut paper = None;
    for (h, b) in &t.blocks {
        if b.name.eq_ignore_ascii_case("*Model_Space") {
            model.get_or_insert(*h);
        } else if b.name.eq_ignore_ascii_case("*Paper_Space") {
            paper.get_or_insert(*h);
        }
    }
    let is_block: HashMap<u64, usize> = t
        .blocks
        .iter()
        .enumerate()
        .map(|(i, (h, _))| (*h, i))
        .collect();
    let index: HashMap<u64, usize> = ents.iter().enumerate().map(|(i, e)| (e.h, i)).collect();
    // Owner of each entity: a block record (top level) or a parent entity (vertex, attribute).
    let mut per_block: Vec<Vec<usize>> = vec![vec![]; t.blocks.len()];
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    // Model/paper-space entities whose block record did not decode: (index, paper).
    let mut orphans: Vec<(usize, bool)> = vec![];
    for (i, e) in ents.iter().enumerate() {
        let owner = match e.com.mode {
            2 => model,
            1 => paper,
            _ => Some(e.com.owner),
        };
        let Some(owner) = owner else {
            orphans.push((i, e.com.mode == 1));
            continue;
        };
        if let Some(&b) = is_block.get(&owner) {
            per_block[b].push(i);
        } else {
            children.entry(owner).or_default().push(e.h);
        }
    }
    // An entity that names a block as owner but is not in that block's entity list (R2004+
    // owned list, or a complete R13–R2000 chain) is unreachable: pro BIM tools drop it too.
    let mut unowned = 0usize;
    // BLOCK / ENDBLK are referenced by the record itself, never listed.
    let framing = |i: usize| matches!(ents[i].kind, Kind::Block | Kind::EndBlk);
    for (b, list) in per_block.iter_mut().enumerate() {
        let rec = &t.blocks[b].1;
        if !rec.owned.is_empty() {
            let pos: HashMap<u64, usize> =
                rec.owned.iter().enumerate().map(|(i, h)| (*h, i)).collect();
            let listed = |i: usize| pos.contains_key(&ents[i].h);
            let real = list.iter().filter(|&&i| !framing(i)).count();
            let hits = list.iter().filter(|&&i| listed(i)).count();
            // A list that matches under half of its members is not trusted (kept as is).
            if hits * 2 >= real {
                let n = list.len();
                list.retain(|&i| framing(i) || listed(i));
                unowned += n - list.len();
            }
            list.sort_by_key(|&i| pos.get(&ents[i].h).copied().unwrap_or(usize::MAX));
        } else if let Some((first, last)) = rec.chain {
            let set: HashSet<usize> = list.iter().copied().collect();
            let chain = walk_chain(&index, &ents, first, last, |i| set.contains(&i));
            if chain.last().is_some_and(|&i| ents[i].h == last) {
                let on: HashSet<usize> = chain.iter().copied().collect();
                unowned += list
                    .iter()
                    .filter(|&&i| !framing(i) && !on.contains(&i))
                    .count();
                *list = chain;
            } else {
                let ordered = chain_order(list, &chain);
                *list = ordered;
            }
        }
    }
    if unowned > 0 {
        *rep.skipped.entry("unowned entity".to_string()).or_default() += unowned;
        rep.warn(format!(
            "{unowned} entities not in their block's entity list dropped"
        ));
    }
    if model.is_none() {
        if ents.is_empty() {
            return Err(err(
                "model space block record missing or undecodable; no drawing content recovered",
            ));
        }
        rep.warn(format!(
            "model space block record missing or undecodable; {} entities placed in model space",
            orphans.iter().filter(|o| !o.1).count()
        ));
    }
    if paper.is_none() && orphans.iter().any(|o| o.1) {
        rep.warn("paper space block record missing; its entities kept as paper-space entities");
    }
    let ctx = Ctx {
        t,
        ents: &index,
        all: &ents,
        children: &children,
        blocks: t
            .blocks
            .iter()
            .map(|(h, b)| (*h, canonical(&b.name)))
            .collect(),
        unresolved: Cell::new(0),
        tables_no_block: Cell::new(0),
    };
    for &h in &t.order.1 {
        let l = &t.ltypes[&h];
        doc.ltypes.push(LineType {
            name: l.name.clone(),
            description: l.desc.clone(),
            flags: 0,
            pattern_length: l.length,
            pattern: l.dashes.clone(),
            extra: vec![],
        });
    }
    for &h in &t.order.0 {
        let l = &t.layers[&h];
        let mut extra = vec![];
        if let Some(c) = l.true_color {
            extra.push(p(420, c.to_string()));
        }
        match l.lw_index {
            Some(i) if (i as usize) < LW.len() => extra.push(p(370, LW[i as usize].to_string())),
            Some(31) => extra.push(p(370, "-3")),
            _ => {}
        }
        let color = if l.color == 0 { 7 } else { l.color };
        doc.layers.push(Layer {
            name: l.name.clone(),
            flags: l.flags,
            color: if l.off { -color } else { color },
            linetype: t
                .ltypes
                .get(&l.ltype)
                .map(|x| x.name.clone())
                .unwrap_or_else(|| "Continuous".into()),
            extra,
        });
    }
    for &h in &t.order.2 {
        let s = &t.styles[&h];
        doc.styles.push(TextStyle {
            name: s.name.clone(),
            flags: s.flags,
            height: s.height,
            width_factor: s.width,
            oblique: s.oblique.to_degrees(),
            font: s.font.clone(),
            big_font: s.big_font.clone(),
            extra: vec![],
        });
    }
    for (b, (h, rec)) in t.blocks.iter().enumerate() {
        let is_model = Some(*h) == model;
        let is_paper = Some(*h) == paper;
        let mut list = vec![];
        for &i in &per_block[b] {
            ctx.convert(&ents[i], is_paper, &mut list);
        }
        rep.entities += list.len();
        if is_model {
            doc.entities.extend(list);
            continue;
        }
        let mut blk = Block::new(canonical(&rec.name));
        blk.flags = rec.flags;
        blk.base = rec.base;
        if let Some(&i) = index.get(&rec.block_ent) {
            blk.layer = ctx.common(&ents[i].com, false).layer;
        }
        for e in list {
            match e {
                Entity::AttDef(a) => blk.attdefs.push(a),
                e => blk.entities.push(e),
            }
        }
        if is_paper && blk.entities.is_empty() && blk.attdefs.is_empty() {
            continue;
        }
        doc.blocks.push(blk);
    }
    let mut list = vec![];
    for &(i, is_paper) in &orphans {
        ctx.convert(&ents[i], is_paper, &mut list);
    }
    rep.entities += list.len();
    doc.entities.extend(list);
    if !t.imagedefs.is_empty() {
        doc.other_sections.push(image_objects(t));
    }
    let n = ctx.unresolved.get();
    if n > 0 {
        *rep.skipped
            .entry("INSERT(unresolved block)".to_string())
            .or_default() += n;
    }
    let n = ctx.tables_no_block.get();
    if n > 0 {
        *rep.skipped
            .entry("ACAD_TABLE(no block)".to_string())
            .or_default() += n;
        rep.warn(format!(
            "{n} table(s) skipped: no rendered block to draw"
        ));
    }
    Ok(doc)
}

fn canonical(name: &str) -> &str {
    if name.eq_ignore_ascii_case("*Model_Space") {
        "*Model_Space"
    } else if name.eq_ignore_ascii_case("*Paper_Space") {
        "*Paper_Space"
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn com(mode: u8, owner: u64, next: u64) -> ECom {
        ECom {
            mode,
            owner,
            layer: 0,
            lt: Lt::ByLayer,
            color: 256,
            true_color: None,
            lw_index: None,
            ltscale: 1.0,
            invisible: false,
            next,
        }
    }

    fn ent(h: u64, c: ECom, kind: Kind) -> Ent {
        Ent { h, com: c, kind }
    }

    fn line(h: u64, c: ECom) -> Ent {
        let kind = Kind::Line {
            a: [h as f64, 0.0, 0.0],
            b: [0.0; 3],
            th: 0.0,
            ext: [0.0, 0.0, 1.0],
        };
        ent(h, c, kind)
    }

    fn vertex(h: u64, owner: u64, next: u64) -> Ent {
        let kind = Kind::Vertex {
            flags: 0,
            p: [h as f64, 0.0, 0.0],
            w0: 0.0,
            w1: 0.0,
            bulge: 0.0,
        };
        ent(h, com(0, owner, next), kind)
    }

    fn block(h: u64, name: &str, chain: Option<(u64, u64)>) -> (u64, o::BlockRec) {
        let rec = o::BlockRec {
            name: name.into(),
            flags: 0,
            base: [0.0; 3],
            block_ent: 0,
            owned: vec![],
            chain,
        };
        (h, rec)
    }

    fn run(t: &Tables, ents: Vec<Ent>, rep: &mut DwgReport) -> R<DxfDocument> {
        assemble(
            t,
            ents,
            Version::R2000,
            &header::HeaderInfo::default(),
            30,
            rep,
        )
    }

    fn poly(owned: Owned) -> Ent {
        let kind = Kind::Poly2d {
            flags: 0,
            w0: 0.0,
            w1: 0.0,
            th: 0.0,
            elev: 0.0,
            ext: [0.0, 0.0, 1.0],
            owned,
        };
        ent(10, com(2, 0, 0), kind)
    }

    fn xs(doc: &DxfDocument) -> Vec<f64> {
        match &doc.entities[0] {
            Entity::LwPolyline(l) => l.vertices.iter().map(|v| v.x).collect(),
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn polyline_vertices_follow_link_chain() {
        let t = Tables {
            blocks: vec![block(1, "*Model_Space", None)],
            ..Default::default()
        };
        // Handle-map order 22, 20, 21 (vertex 20 inserted by an edit); chain 20 -> 21 -> 22.
        let ents = vec![
            poly(Owned::Chain(20, 22)),
            vertex(22, 10, 23),
            vertex(20, 10, 21),
            vertex(21, 10, 22),
        ];
        let doc = run(&t, ents, &mut DwgReport::default()).unwrap();
        assert_eq!(xs(&doc), vec![20.0, 21.0, 22.0]);
        // A cycle (22 -> 20) with an unreachable last handle terminates; nothing is lost.
        let ents = vec![
            poly(Owned::Chain(20, 99)),
            vertex(22, 10, 20),
            vertex(20, 10, 21),
            vertex(21, 10, 22),
        ];
        let doc = run(&t, ents, &mut DwgReport::default()).unwrap();
        assert_eq!(xs(&doc), vec![20.0, 21.0, 22.0]);
        // Broken chain: the vertices it misses keep stored order after it.
        let ents = vec![
            poly(Owned::Chain(21, 22)),
            vertex(22, 10, 0),
            vertex(20, 10, 0),
            vertex(21, 10, 0),
        ];
        let doc = run(&t, ents, &mut DwgReport::default()).unwrap();
        assert_eq!(xs(&doc), vec![21.0, 22.0, 20.0]);
    }

    #[test]
    fn block_entities_follow_link_chain() {
        let t = Tables {
            blocks: vec![
                block(1, "*Model_Space", None),
                block(2, "B", Some((31, 33))),
            ],
            ..Default::default()
        };
        let ents = vec![
            line(33, com(0, 2, 0)),
            line(31, com(0, 2, 32)),
            line(32, com(0, 2, 33)),
        ];
        let doc = run(&t, ents, &mut DwgReport::default()).unwrap();
        let b = doc.blocks.iter().find(|b| b.name == "B").unwrap();
        let order: Vec<f64> = b
            .entities
            .iter()
            .map(|e| match e {
                Entity::Line(l) => l.start[0],
                e => panic!("{e:?}"),
            })
            .collect();
        assert_eq!(order, vec![31.0, 32.0, 33.0]);
    }

    #[test]
    fn missing_model_space_recovers_or_errors() {
        let t = Tables::default();
        let mut rep = DwgReport::default();
        let doc = run(
            &t,
            vec![line(5, com(2, 0, 0)), line(6, com(1, 0, 0))],
            &mut rep,
        )
        .unwrap();
        assert_eq!(doc.entities.len(), 2);
        assert!(
            rep.warnings.iter().any(|w| w.contains("model space")),
            "{:?}",
            rep.warnings
        );
        assert!(rep.warnings.iter().any(|w| w.contains("paper space")));
        let e = run(&t, vec![], &mut DwgReport::default()).unwrap_err();
        assert!(e.0.contains("model space"), "{}", e.0);
    }

    #[test]
    fn unresolved_insert_counted() {
        let t = Tables {
            blocks: vec![block(1, "*Model_Space", None)],
            ..Default::default()
        };
        let kind = Kind::Insert {
            ins: [0.0; 3],
            scale: [1.0; 3],
            rot: 0.0,
            ext: [0.0, 0.0, 1.0],
            block: 777,
            attribs: Owned::List(vec![]),
            grid: None,
        };
        let mut rep = DwgReport::default();
        let doc = run(&t, vec![ent(5, com(2, 0, 0), kind)], &mut rep).unwrap();
        assert!(doc.entities.is_empty());
        assert_eq!(rep.skipped.get("INSERT(unresolved block)"), Some(&1));
    }

    fn table_doc(target: u64) -> (DxfDocument, DwgReport) {
        let t = Tables {
            blocks: vec![block(1, "*Model_Space", None), block(0x30, "*T1", None)],
            ..Default::default()
        };
        let kind = Kind::Table {
            ins: [3.0, 4.0, 0.0],
            rot: std::f64::consts::FRAC_PI_2,
            ext: [0.0, 0.0, 1.0],
            block: target,
        };
        let mut rep = DwgReport::default();
        let doc = run(&t, vec![ent(5, com(2, 0, 0), kind)], &mut rep).unwrap();
        (doc, rep)
    }

    #[test]
    fn dwg_table_drawn_from_block() {
        let (doc, rep) = table_doc(0x30);
        let Some(Entity::Insert(i)) = doc.entities.first() else {
            panic!("{:?}", doc.entities);
        };
        assert_eq!((i.block.as_str(), i.pos, i.scale), ("*T1", [3.0, 4.0, 0.0], [1.0; 3]));
        assert!((i.rotation - 90.0).abs() < 1e-9);
        assert!(rep.skipped.get("ACAD_TABLE(no block)").is_none());
    }

    #[test]
    fn table_block_content_drawn_once() {
        let t = Tables {
            blocks: vec![block(1, "*Model_Space", None), block(0x30, "*T1", None)],
            ..Default::default()
        };
        let kind = Kind::Table {
            ins: [0.0; 3],
            rot: 0.0,
            ext: [0.0, 0.0, 1.0],
            block: 0x30,
        };
        let ents = vec![ent(5, com(2, 0, 0), kind), line(40, com(0, 0x30, 0))];
        let doc = run(&t, ents, &mut DwgReport::default()).unwrap();
        // Model space holds only the reference; the block holds the single line.
        assert_eq!(doc.entities.len(), 1);
        assert!(matches!(doc.entities[0], Entity::Insert(_)));
        let b = doc.blocks.iter().find(|b| b.name == "*T1").unwrap();
        assert_eq!(b.entities.len(), 1);
    }

    #[test]
    fn table_missing_block_counted_skipped() {
        let (doc, rep) = table_doc(777);
        assert!(doc.entities.is_empty());
        assert_eq!(rep.skipped.get("ACAD_TABLE(no block)"), Some(&1));
    }

    #[test]
    fn hatch_pattern_lines_emitted() {
        let t = Tables {
            blocks: vec![block(1, "*Model_Space", None)],
            ..Default::default()
        };
        let kind = Kind::Hatch {
            elev: 0.0,
            ext: [0.0, 0.0, 1.0],
            name: "ANSI31".into(),
            solid: false,
            assoc: false,
            paths: vec![],
            style: 0,
            ptype: 1,
            angle: 0.0,
            scale: 1.0,
            double: false,
            lines: vec![o::PatLine {
                angle: 45.0,
                base: [0.0, 0.0],
                offset: [-2.0, 2.0],
                dashes: vec![0.5, -0.25],
            }],
            seeds: vec![],
        };
        let doc = run(
            &t,
            vec![ent(5, com(2, 0, 0), kind)],
            &mut DwgReport::default(),
        )
        .unwrap();
        let Entity::Unknown(v) = &doc.entities[0] else {
            panic!("{:?}", doc.entities[0]);
        };
        let at = |c: i32| v.iter().position(|q| q.code == c).unwrap();
        let val = |c: i32| v[at(c)].value.parse::<f64>().unwrap();
        assert_eq!(
            (val(78), val(53), val(45), val(46), val(79)),
            (1.0, 45.0, -2.0, 2.0, 2.0)
        );
        assert_eq!(v.iter().filter(|q| q.code == 49).count(), 2);
        assert!(at(41) < at(78) && at(78) < at(53) && at(49) < at(98));
    }

    fn model(kind: Kind, t: &Tables) -> Vec<Pair> {
        let doc = run(
            t,
            vec![ent(5, com(2, 0, 0), kind)],
            &mut DwgReport::default(),
        )
        .unwrap();
        match &doc.entities[0] {
            Entity::Unknown(v) => v.clone(),
            e => panic!("{e:?}"),
        }
    }

    fn val(v: &[Pair], code: i32) -> &str {
        v.iter()
            .find(|q| q.code == code)
            .map_or("", |q| q.value.as_str())
    }

    #[test]
    fn leader_record_with_dimstyle() {
        let mut t = Tables {
            blocks: vec![block(1, "*Model_Space", None)],
            ..Default::default()
        };
        t.dimstyles.insert(0x27, "ISO-25".into());
        let d = annot::LeaderD {
            annot: 0,
            path: 0,
            pts: vec![[0.0; 3], [10.0, 5.0, 0.0]],
            ext: [0.0, 0.0, 1.0],
            xdir: [1.0, 0.0, 0.0],
            block_off: [0.0; 3],
            annot_off: [0.5, 0.0, 0.0],
            box_h: 0.0,
            box_w: 0.0,
            hook: false,
            arrow: true,
            style: 0x27,
        };
        let v = model(Kind::Leader(Box::new(d)), &t);
        assert_eq!(
            (val(&v, 0), val(&v, 3), val(&v, 71), val(&v, 76)),
            ("LEADER", "ISO-25", "1", "2")
        );
        assert_eq!(v.iter().filter(|q| q.code == 10).count(), 2);
        assert_eq!(val(&v, 213), "0.5");
        assert!(v.iter().all(|q| q.code != 40 && q.code != 210));
    }

    #[test]
    fn image_record_and_definition_objects() {
        let mut t = Tables {
            blocks: vec![block(1, "*Model_Space", None)],
            max_handle: 0x40,
            ..Default::default()
        };
        let def = ImageDef {
            size: [640.0, 480.0],
            path: "C:\\img\\site.png".into(),
            loaded: true,
            units: 2,
            pixel: [0.01, 0.01],
        };
        t.imagedefs.insert(0x30, def);
        let d = annot::ImageD {
            origin: [100.0, 50.0, 0.0],
            u: [0.5, 0.0, 0.0],
            v: [0.0, 0.5, 0.0],
            size: [640.0, 480.0],
            flags: 7,
            clip: false,
            bright: 50,
            contrast: 50,
            fade: 0,
            clip_type: 1,
            clip_pts: vec![[-0.5, -0.5], [639.5, 479.5]],
            def: 0x30,
        };
        let kind = Kind::Image {
            wipeout: false,
            d: Box::new(d),
        };
        let doc = run(
            &t,
            vec![ent(5, com(2, 0, 0), kind)],
            &mut DwgReport::default(),
        )
        .unwrap();
        let Entity::Unknown(v) = &doc.entities[0] else {
            panic!("{:?}", doc.entities[0]);
        };
        assert_eq!(
            (val(v, 0), val(v, 340), val(v, 13), val(v, 23)),
            ("IMAGE", "30", "640", "480")
        );
        let obj = &doc.other_sections[0];
        assert_eq!(obj.name, "OBJECTS");
        let at = |code: i32, value: &str| {
            obj.pairs
                .iter()
                .position(|q| q.code == code && q.value == value)
        };
        assert!(at(5, "41").is_some() && at(3, "ACAD_IMAGE_DICT").is_some());
        assert!(at(3, "site").unwrap() < at(0, "IMAGEDEF").unwrap());
        let paths = crate::dxf::annot::image_paths(&doc);
        assert_eq!(
            paths.get("30").map(String::as_str),
            Some("C:\\img\\site.png")
        );
        // Without definitions no OBJECTS section is added.
        let t = Tables {
            blocks: vec![block(1, "*Model_Space", None)],
            ..Default::default()
        };
        let kind = Kind::Acis {
            name: "3DSOLID",
            sat: Some("400 0 1 0\npoint $-1 1 2 3 #".into()),
        };
        let doc = run(
            &t,
            vec![ent(5, com(2, 0, 0), kind)],
            &mut DwgReport::default(),
        )
        .unwrap();
        assert!(doc.other_sections.is_empty());
        let Entity::Unknown(v) = &doc.entities[0] else {
            panic!("{:?}", doc.entities[0]);
        };
        let lines: Vec<String> = v
            .iter()
            .filter(|q| q.code == 1)
            .map(|q| crate::dxf::annot::sat_crypt(&q.value))
            .collect();
        assert_eq!(lines, vec!["400 0 1 0", "point $-1 1 2 3 #"]);
    }

    #[test]
    fn layouts_from_dictionary() {
        let dicts = vec![
            vec![("ACAD_GROUP".to_string(), 50)],
            vec![
                ("Layout1".to_string(), 101),
                ("Model".to_string(), 100),
                ("Junk".to_string(), 102),
            ],
        ];
        let types = HashMap::from([(50, 42), (100, 500), (101, 500), (102, 42)]);
        let classes = HashMap::from([(500, "LAYOUT".to_string())]);
        assert_eq!(
            layout_names(&dicts, &types, &classes),
            vec!["Model", "Layout1"]
        );
        assert!(layout_names(&dicts[..1], &types, &classes).is_empty());
    }

    fn starts(doc: &DxfDocument) -> Vec<f64> {
        doc.entities
            .iter()
            .map(|e| match e {
                Entity::Line(l) => l.start[0],
                e => panic!("{e:?}"),
            })
            .collect()
    }

    #[test]
    fn unowned_entities_dropped_by_owned_list() {
        let mut ms = block(1, "*Model_Space", None);
        ms.1.owned = vec![6, 5];
        let mut b = block(2, "B", None);
        b.1.owned = vec![31];
        let t = Tables {
            blocks: vec![ms, b],
            ..Default::default()
        };
        let ents = vec![
            line(5, com(2, 0, 0)),
            line(6, com(2, 0, 0)),
            line(7, com(2, 0, 0)),
            ent(30, com(0, 2, 0), Kind::Block),
            line(31, com(0, 2, 0)),
            line(32, com(0, 2, 0)),
        ];
        let mut rep = DwgReport::default();
        let doc = run(&t, ents, &mut rep).unwrap();
        assert_eq!(starts(&doc), vec![6.0, 5.0]);
        let blk = doc.blocks.iter().find(|b| b.name == "B").unwrap();
        assert_eq!(blk.entities.len(), 1);
        // BLOCK is never listed and is not counted.
        assert_eq!(rep.skipped.get("unowned entity"), Some(&2));
        assert!(rep
            .warnings
            .iter()
            .any(|w| w.contains("not in their block")));
        // A list that matches almost none of its members is not trusted: nothing dropped.
        let mut ms = block(1, "*Model_Space", None);
        ms.1.owned = vec![5, 99, 98];
        let t = Tables {
            blocks: vec![ms],
            ..Default::default()
        };
        let ents = (5..9).map(|h| line(h, com(2, 0, 0))).collect();
        let mut rep = DwgReport::default();
        let doc = run(&t, ents, &mut rep).unwrap();
        assert_eq!(doc.entities.len(), 4);
        assert!(rep.skipped.get("unowned entity").is_none());
    }

    #[test]
    fn unowned_entities_dropped_only_by_complete_chain() {
        let ents = || {
            vec![
                line(5, com(2, 0, 6)),
                line(6, com(2, 0, 0)),
                line(7, com(2, 0, 0)),
            ]
        };
        let t = Tables {
            blocks: vec![block(1, "*Model_Space", Some((5, 6)))],
            ..Default::default()
        };
        let mut rep = DwgReport::default();
        let doc = run(&t, ents(), &mut rep).unwrap();
        assert_eq!(starts(&doc), vec![5.0, 6.0]);
        assert_eq!(rep.skipped.get("unowned entity"), Some(&1));
        // The chain does not reach its last handle (an entity that did not decode): keep all.
        let t = Tables {
            blocks: vec![block(1, "*Model_Space", Some((5, 9)))],
            ..Default::default()
        };
        let mut rep = DwgReport::default();
        let doc = run(&t, ents(), &mut rep).unwrap();
        assert_eq!(starts(&doc), vec![5.0, 6.0, 7.0]);
        assert!(rep.skipped.get("unowned entity").is_none());
    }

    #[test]
    fn block_reference_class_and_ole_frame_records() {
        let t = Tables {
            blocks: vec![block(1, "*Model_Space", None), block(0x30, "*I333", None)],
            ..Default::default()
        };
        let kind = Kind::BlockRef {
            ins: [-10.5, 2.0, 0.0],
            scale: [2.0, 1.0, 1.0],
            rot: std::f64::consts::FRAC_PI_2,
            ext: [0.0, 0.0, 1.0],
            block: 0x30,
        };
        let v = model(kind, &t);
        assert_eq!(
            (
                val(&v, 0),
                val(&v, 2),
                val(&v, 10),
                val(&v, 41),
                val(&v, 50)
            ),
            ("ACIDBLOCKREFERENCE", "*I333", "-10.5", "2", "90")
        );
        assert!(v.iter().all(|q| q.code != 42 && q.code != 210));
        assert!(v
            .iter()
            .any(|q| q.code == 100 && q.value == "AcDbBlockReference"));
        let v = model(Kind::Ole { version: 2 }, &t);
        assert_eq!(
            (val(&v, 0), val(&v, 70), val(&v, 1)),
            ("OLE2FRAME", "2", "OLE")
        );
    }
}
