//! Bridge between a DXF document and the model's 2D tables. Import is one undoable operation (`Model::batch`).
//! Angles: DXF degrees, model radians. A circle is stored as a full 0..2π arc and written back
//! as CIRCLE. The model does not store Z or vertex widths; line colour/type/weight on an object are stored in columns; MTEXT is stored as d_text with mtext_width.
// the export half lives only with the test-only DXF writer (`dxf-write`)
#![cfg_attr(not(any(test, feature = "dxf-write")), allow(dead_code, unused_imports))]

use std::collections::{BTreeMap, HashMap, HashSet};

use ddc_model::{Error, Model};
use rusqlite::{params, Connection};

use super::{
    Arc, AttDef, Attrib, Block, Circle, Common, DxfDocument, Entity, Insert, Layer, Line,
    LineType, LwPolyline, LwVertex, MText, Pair, Text,
};
use super::ltypes;
use super::text::mtext_lines;

const TAU: f64 = std::f64::consts::TAU;
const DEFAULT_WEIGHT: f64 = 0.25;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ImportReport {
    pub layers: usize,
    pub blocks: usize,
    pub lines: usize,
    pub polylines: usize,
    pub arcs: usize,
    pub circles: usize,
    pub texts: usize,
    pub inserts: usize,
    pub attributes: usize,
    /// Hatches and fills (outlines are polylines with a pattern).
    pub hatches: usize,
    /// Unrecognised entities (and attribute inserts/definitions outside a supported place), skipped.
    pub skipped_unknown: usize,
    /// Inserts of an unknown block, skipped.
    pub skipped_inserts: usize,
    /// Block inserts inside a block definition that would make the block contain itself, skipped.
    pub cyclic_inserts: usize,
    /// Blocks whose name exists with other content, imported under a new name (`name~2`).
    pub renamed_blocks: usize,
    /// Blocks whose name and content match an existing block, not imported again.
    pub reused_blocks: usize,
    /// ELLIPSE and SPLINE records turned into editable polylines.
    pub curves: usize,
    /// Dropped entities by kind: spline, ellipse, mtext, dimension, leader, solid, proxy, others.
    pub dropped: BTreeMap<String, usize>,
}

/// Report bucket for a dropped record type.
pub fn dropped_kind(rec: &str) -> &'static str {
    let k = rec.trim().to_ascii_uppercase();
    match k.as_str() {
        "SPLINE" => "spline",
        "ELLIPSE" => "ellipse",
        "MTEXT" => "mtext",
        "DIMENSION" | "ARC_DIMENSION" | "LARGE_RADIAL_DIMENSION" => "dimension",
        "LEADER" | "MULTILEADER" | "MLEADER" | "TOLERANCE" => "leader",
        "SOLID" | "TRACE" | "3DSOLID" | "REGION" | "BODY" | "SURFACE" => "solid",
        _ if k.contains("PROXY") || k.starts_with("ACAD_") || k.starts_with("ACDB") => "proxy",
        _ => "others",
    }
}

struct Ctx<'a> {
    db: &'a Connection,
    layers: HashMap<String, i64>,
    blocks: HashMap<String, i64>,
    /// Block id (new rows of this import) to the name it has in the document.
    owner_names: HashMap<i64, String>,
    /// (owner block, inserted block) pairs of the document that close a reference cycle.
    cyclic: HashSet<(String, String)>,
    rep: ImportReport,
    /// IMAGEDEF handle to file path, for image stand-ins.
    images: HashMap<String, String>,
    /// Max chord sag for lowered curves, drawing units (infinite when the unit is unknown).
    sag: f64,
}

type P3 = [f64; 3];

/// NaN/inf would bind NULL into NOT NULL columns and abort the whole import.
fn all_finite(v: &[f64]) -> bool {
    v.iter().all(|x| x.is_finite())
}

impl Ctx<'_> {
    fn drop_kind(&mut self, rec: &str) {
        self.rep.skipped_unknown += 1;
        *self
            .rep
            .dropped
            .entry(dropped_kind(rec).to_string())
            .or_default() += 1;
    }

    fn layer(&mut self, name: &str) -> Result<i64, Error> {
        if let Some(&id) = self.layers.get(name) {
            return Ok(id);
        }
        self.add_layer(name, 7, "CONTINUOUS", DEFAULT_WEIGHT, None)
    }

    fn add_layer(
        &mut self,
        name: &str,
        color: i32,
        lt: &str,
        weight: f64,
        true_color: Option<i32>,
    ) -> Result<i64, Error> {
        self.db
            .prepare_cached(
                "INSERT INTO d_layer(name,color,linetype,weight,true_color) VALUES (?1,?2,?3,?4,?5)",
            )?
            .execute(params![name, color, lt, weight, true_color])?;
        let id = self.db.last_insert_rowid();
        self.layers.insert(name.to_string(), id);
        self.rep.layers += 1;
        Ok(id)
    }

    /// An entity in model space (`block` = None) or in a block definition.
    fn entity(&mut self, e: &Entity, block: Option<i64>) -> Result<(), Error> {
        let db = self.db;
        match e {
            Entity::Line(l) => {
                if !all_finite(&[l.start[0], l.start[1], l.end[0], l.end[1]]) {
                    self.drop_kind("LINE");
                    return Ok(());
                }
                let ly = self.layer(&l.common.layer)?;
                db.prepare_cached(
                    "INSERT INTO d_line(layer,block,x1,y1,x2,y2) VALUES (?1,?2,?3,?4,?5,?6)",
                )?
                .execute(params![
                    ly, block, l.start[0], l.start[1], l.end[0], l.end[1]
                ])?;
                self.style("d_line", &l.common)?;
                self.rep.lines += 1;
            }
            Entity::LwPolyline(p) => {
                if !p.vertices.iter().all(|v| all_finite(&[v.x, v.y, v.bulge])) {
                    self.drop_kind("LWPOLYLINE");
                    return Ok(());
                }
                let ly = self.layer(&p.common.layer)?;
                db.prepare_cached("INSERT INTO d_polyline(layer,block,closed) VALUES (?1,?2,?3)")?
                    .execute(params![ly, block, p.closed()])?;
                let id = db.last_insert_rowid();
                self.style("d_polyline", &p.common)?;
                let mut st = db.prepare_cached("INSERT INTO d_vertex VALUES (?1,?2,?3,?4,?5)")?;
                for (i, v) in p.vertices.iter().enumerate() {
                    st.execute(params![id, i as i64, v.x, v.y, v.bulge])?;
                }
                self.rep.polylines += 1;
            }
            Entity::Arc(a) => {
                if !(a.radius.is_finite() && a.radius > 0.0)
                    || !all_finite(&[a.center[0], a.center[1], a.start_angle, a.end_angle])
                {
                    self.drop_kind("ARC");
                    return Ok(());
                }
                let ly = self.layer(&a.common.layer)?;
                let (a0, a1) = (a.start_angle.to_radians(), a.end_angle.to_radians());
                self.arc(ly, block, a.center, a.radius, a0, a1)?;
                self.style("d_arc", &a.common)?;
                self.rep.arcs += 1;
            }
            Entity::Circle(c) => {
                if !(c.radius.is_finite() && c.radius > 0.0) || !all_finite(&[c.center[0], c.center[1]])
                {
                    self.drop_kind("CIRCLE");
                    return Ok(());
                }
                let ly = self.layer(&c.common.layer)?;
                self.arc(ly, block, c.center, c.radius, 0.0, TAU)?;
                self.style("d_arc", &c.common)?;
                self.rep.circles += 1;
            }
            Entity::Text(t) => {
                if !all_finite(&[t.pos[0], t.pos[1], t.height, t.rotation]) {
                    self.drop_kind("TEXT");
                    return Ok(());
                }
                self.text(&t.common, block, t.pos, t.height, t.rotation, &t.text)?
            }
            Entity::MText(t) => {
                if !all_finite(&[t.pos[0], t.pos[1], t.height, t.rotation]) {
                    self.drop_kind("MTEXT");
                    return Ok(());
                }
                self.text(&t.common, block, t.pos, t.height, t.rotation, &mtext_lines(&t.text))?;
                self.db
                    .prepare_cached("UPDATE d_text SET mtext_width=?2,attach=?3 WHERE id=?1")?
                    .execute(params![
                        self.db.last_insert_rowid(),
                        t.width,
                        t.attach.unwrap_or(1)
                    ])?;
            }
            Entity::Insert(i) => self.insert(i, block)?,
            Entity::Unknown(r) if r[0].value == "HATCH" => {
                let Some(f) = super::lower::hatch(r) else {
                    self.drop_kind("HATCH");
                    return Ok(());
                };
                if !f.loops.iter().all(|l| l.iter().all(|v| all_finite(&[v.0, v.1]))) {
                    self.drop_kind("HATCH");
                    return Ok(());
                }
                let ly = self.layer(&f.common.layer)?;
                let pattern = if f.solid {
                    "SOLID".to_string()
                } else {
                    f.pattern.clone()
                };
                let mut first: Option<i64> = None;
                for l in &f.loops {
                    db.prepare_cached(
                        "INSERT INTO d_polyline(layer,block,closed,hatch,hatch_id) VALUES (?1,?2,1,?3,?4)",
                    )?
                    .execute(params![ly, block, pattern, first])?;
                    let id = db.last_insert_rowid();
                    if first.is_none() {
                        first = Some(id);
                        db.prepare_cached("UPDATE d_polyline SET hatch_id=?1 WHERE id=?1")?
                            .execute([id])?;
                    }
                    self.style("d_polyline", &f.common)?;
                    let mut st =
                        db.prepare_cached("INSERT INTO d_vertex VALUES (?1,?2,?3,?4,0)")?;
                    for (i, v) in l.iter().enumerate() {
                        st.execute(params![id, i as i64, v.0, v.1])?;
                    }
                }
                self.rep.hatches += 1;
            }
            Entity::Unknown(p) => {
                let kind = p[0].value.trim();
                if matches!(kind, "ELLIPSE" | "SPLINE") {
                    match super::lower::curve(p, self.sag) {
                        Some(x) => {
                            self.entity(&x, block)?;
                            self.rep.curves += 1;
                        }
                        None => self.drop_kind(kind),
                    }
                    return Ok(());
                }
                // Leaders, images and solids are kept as their plan stand-ins.
                let low = super::annot::lower(p, &self.images);
                if low.is_empty() {
                    self.drop_kind(kind);
                }
                for x in &low {
                    self.entity(x, block)?;
                }
            }
            Entity::Insert(_) | Entity::AttDef(_) => self.drop_kind("INSERT"),
        }
        Ok(())
    }

    fn arc(
        &mut self,
        ly: i64,
        block: Option<i64>,
        c: P3,
        r: f64,
        a0: f64,
        a1: f64,
    ) -> Result<(), Error> {
        if r <= 0.0 {
            return Err(Error::Invalid("radius must be positive".into()));
        }
        self.db
            .prepare_cached(
                "INSERT INTO d_arc(layer,block,cx,cy,r,a0,a1) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            )?
            .execute(params![ly, block, c[0], c[1], r, a0, a1])?;
        Ok(())
    }

    fn text(
        &mut self,
        c: &Common,
        block: Option<i64>,
        pos: P3,
        h: f64,
        rot: f64,
        s: &str,
    ) -> Result<(), Error> {
        let ly = self.layer(&c.layer)?;
        self.db
            .prepare_cached(
                "INSERT INTO d_text(layer,block,x,y,height,rotation,content) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
            )?
            .execute(params![ly, block, pos[0], pos[1], h, rot.to_radians(), s])?;
        self.style("d_text", c)?;
        self.rep.texts += 1;
        Ok(())
    }

    /// Style of the last inserted row of `table`; empty fields stay NULL ("by layer").
    fn style(&self, table: &str, c: &Common) -> Result<(), Error> {
        if c.color.is_none()
            && c.true_color.is_none()
            && c.linetype.is_none()
            && c.lineweight.is_none()
        {
            return Ok(());
        }
        self.db
            .prepare_cached(&format!(
                "UPDATE {table} SET color=?2,true_color=?3,linetype=?4,lineweight=?5 WHERE id=?1"
            ))?
            .execute(params![
                self.db.last_insert_rowid(),
                c.color,
                c.true_color,
                c.linetype,
                c.lineweight
            ])?;
        Ok(())
    }

    fn insert(&mut self, i: &Insert, owner: Option<i64>) -> Result<(), Error> {
        if let Some(o) = owner.and_then(|o| self.owner_names.get(&o)) {
            if self.cyclic.contains(&(o.clone(), i.block.clone())) {
                self.rep.cyclic_inserts += 1;
                return Ok(());
            }
        }
        let Some(&def) = self.blocks.get(&i.block) else {
            self.rep.skipped_inserts += 1;
            return Ok(());
        };
        let ly = self.layer(&i.common.layer)?;
        let db = self.db;
        db.prepare_cached(
            "INSERT INTO d_insert(layer,def,x,y,sx,sy,rotation,block) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        )?
        .execute(params![
            ly,
            def,
            i.pos[0],
            i.pos[1],
            i.scale[0],
            i.scale[1],
            i.rotation.to_radians(),
            owner
        ])?;
        let id = db.last_insert_rowid();
        self.style("d_insert", &i.common)?;
        // Insert values; missing tags take the default from the block definition.
        let mut st = db.prepare_cached("INSERT OR REPLACE INTO d_attr VALUES (?1,?2,?3)")?;
        let mut defs =
            db.prepare_cached("SELECT tag, default_value FROM d_attdef WHERE block=?1")?;
        let defaults: Vec<(String, String)> = defs
            .query_map([def], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        for (tag, val) in &defaults {
            if !i.attribs.iter().any(|a| &a.tag == tag) {
                st.execute(params![id, tag, val])?;
                self.rep.attributes += 1;
            }
        }
        for a in &i.attribs {
            st.execute(params![id, a.tag, a.value])?;
            self.rep.attributes += 1;
        }
        self.rep.inserts += 1;
        Ok(())
    }
}

/// Valid values of code 370 (hundredths of mm); CAD rejects the others.
const LINEWEIGHTS: [i32; 24] = [
    0, 5, 9, 13, 15, 18, 20, 25, 30, 35, 40, 50, 53, 60, 70, 80, 90, 100, 106, 120, 140, 158, 200,
    211,
];

/// Weight in mm → the nearest valid code 370 value; NaN and negative — -3 (default).
fn snap_lineweight(mm: f64) -> i32 {
    if !mm.is_finite() || mm < 0.0 {
        return -3;
    }
    let h = mm * 100.0;
    *LINEWEIGHTS
        .iter()
        .min_by(|a, b| {
            (f64::from(**a) - h)
                .abs()
                .total_cmp(&(f64::from(**b) - h).abs())
        })
        .unwrap()
}

/// Layer line weight: code 370 (hundredths of mm) → mm.
fn layer_weight(l: &Layer) -> f64 {
    l.extra
        .iter()
        .find(|p| p.code == 370)
        .and_then(|p| p.value.trim().parse::<f64>().ok())
        .filter(|w| *w >= 0.0)
        .map_or(DEFAULT_WEIGHT, |w| w / 100.0)
}

/// Layer true colour: code 420 (0x00RRGGBB).
fn layer_true_color(l: &Layer) -> Option<i32> {
    l.extra
        .iter()
        .find(|p| p.code == 420)
        .and_then(|p| p.value.trim().parse::<i32>().ok())
}

/// Imports a document into the model as one journal operation (`Model::undo` removes everything).
/// Existing layers and blocks with the same name are reused unchanged.
pub fn import_into(model: &mut Model, doc: &DxfDocument) -> Result<ImportReport, Error> {
    model.batch("draw_import_dxf", |db| {
        let mut cx = Ctx {
            db,
            layers: HashMap::new(),
            blocks: HashMap::new(),
            owner_names: HashMap::new(),
            cyclic: cyclic_pairs(doc),
            rep: ImportReport::default(),
            images: super::annot::image_paths(doc),
            sag: doc.unit_meters().map_or(f64::INFINITY, |u| 2e-4 / u),
        };
        for (table, map) in [("d_layer", &mut cx.layers), ("d_block", &mut cx.blocks)] {
            let mut st = db.prepare(&format!("SELECT name, id FROM {table}"))?;
            for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
                let (n, id) = r?;
                map.insert(n, id);
            }
        }
        for l in &doc.layers {
            if !cx.layers.contains_key(&l.name) {
                cx.add_layer(&l.name, l.color, &l.linetype, layer_weight(l), layer_true_color(l))?;
            }
        }
        for t in doc.ltypes.iter().filter(|t| !ltypes::is_builtin(t.name.trim())) {
            let pat: Vec<String> = t.pattern.iter().map(|v| v.to_string()).collect();
            let len = if t.pattern_length > 0.0 {
                t.pattern_length
            } else {
                ltypes::length(&t.pattern)
            };
            db.execute(
                "INSERT OR IGNORE INTO d_ltype(name,description,length,pattern) VALUES (?1,?2,?3,?4)",
                params![t.name.trim(), t.description, len, pat.join(" ")],
            )?;
        }
        // Service blocks (*Model_Space etc.) are not carried over.
        // A name that exists with other content gets a provisional row `name~N`; it is compared
        // with the existing block after the content is in, and dropped when identical.
        let existing = cx.blocks.clone();
        let mut todo = vec![];
        let mut seen = HashSet::new();
        for b in doc.blocks.iter().filter(|b| !b.name.starts_with('*')) {
            if !seen.insert(b.name.clone()) {
                continue;
            }
            let clash = existing.get(&b.name).copied();
            let mut name = b.name.clone();
            if clash.is_some() {
                let mut n = 2;
                loop {
                    name = format!("{}~{n}", b.name);
                    let taken: i64 = db.query_row(
                        "SELECT count(*) FROM d_block WHERE name=?1",
                        [&name],
                        |r| r.get(0),
                    )?;
                    if taken == 0 {
                        break;
                    }
                    n += 1;
                }
            }
            db.prepare_cached("INSERT INTO d_block(name,bx,by) VALUES (?1,?2,?3)")?
                .execute(params![name, b.base[0], b.base[1]])?;
            let id = db.last_insert_rowid();
            let mut ad = db.prepare_cached("INSERT INTO d_attdef VALUES (?1,?2,?3,?4,?5,?6)")?;
            for a in &b.attdefs {
                ad.execute(params![id, a.tag, a.default, a.pos[0], a.pos[1], a.height])?;
            }
            cx.blocks.insert(b.name.clone(), id);
            cx.owner_names.insert(id, b.name.clone());
            todo.push((b, id, clash));
        }
        for (b, id, _) in &todo {
            for e in &b.entities {
                cx.entity(e, Some(*id))?;
            }
        }
        resolve_clashes(&mut cx, &todo)?;
        cx.rep.blocks += todo.len() - cx.rep.reused_blocks;
        for e in &doc.entities {
            cx.entity(e, None)?;
        }
        Ok(cx.rep)
    })
}

/// (owner, inserted) block-name pairs of the document that lie on a reference cycle (an insert of
/// a block that reaches the owner again, itself included). Strongly connected components, iterative.
fn cyclic_pairs(doc: &DxfDocument) -> HashSet<(String, String)> {
    let blocks: Vec<&Block> = doc.blocks.iter().filter(|b| !b.name.starts_with('*')).collect();
    let idx: HashMap<&str, usize> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.name.as_str(), i))
        .collect();
    let n = blocks.len();
    let adj: Vec<Vec<usize>> = blocks
        .iter()
        .map(|b| {
            b.entities
                .iter()
                .filter_map(|e| match e {
                    Entity::Insert(i) => idx.get(i.block.as_str()).copied(),
                    _ => None,
                })
                .collect()
        })
        .collect();
    let none = usize::MAX;
    let (mut index, mut low, mut comp) = (vec![none; n], vec![0usize; n], vec![none; n]);
    let mut on = vec![false; n];
    let (mut stack, mut counter, mut ncomp) = (vec![], 0usize, 0usize);
    for r in 0..n {
        if index[r] != none {
            continue;
        }
        index[r] = counter;
        low[r] = counter;
        counter += 1;
        stack.push(r);
        on[r] = true;
        let mut call: Vec<(usize, usize)> = vec![(r, 0)];
        while let Some(top) = call.last_mut() {
            let v = top.0;
            if top.1 < adj[v].len() {
                let w = adj[v][top.1];
                top.1 += 1;
                if index[w] == none {
                    index[w] = counter;
                    low[w] = counter;
                    counter += 1;
                    stack.push(w);
                    on[w] = true;
                    call.push((w, 0));
                } else if on[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                call.pop();
                if low[v] == index[v] {
                    while let Some(w) = stack.pop() {
                        on[w] = false;
                        comp[w] = ncomp;
                        if w == v {
                            break;
                        }
                    }
                    ncomp += 1;
                }
                if let Some(p) = call.last() {
                    let u = p.0;
                    low[u] = low[u].min(low[v]);
                }
            }
        }
    }
    let mut out = HashSet::new();
    for (o, b) in blocks.iter().enumerate() {
        for &t in &adj[o] {
            if comp[o] == comp[t] {
                out.insert((b.name.clone(), blocks[t].name.clone()));
            }
        }
    }
    out
}

/// Content of a block as text (layers by name, nested inserts by block name) for comparison, plus
/// the names of the blocks it inserts. Ids and rowids are left out.
fn block_sig(
    db: &Connection,
    id: i64,
    names: &HashMap<i64, String>,
) -> Result<(String, Vec<String>), Error> {
    let mut out = String::new();
    let mut targets = vec![];
    let (bx, by): (f64, f64) = db.query_row("SELECT bx,by FROM d_block WHERE id=?1", [id], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })?;
    out.push_str(&format!("B{bx:?},{by:?}\n"));
    let sty = "quote(e.color)||'|'||quote(e.true_color)||'|'||quote(e.linetype)||'|'||quote(e.lineweight)";
    let simple = [
        ("L", "d_line", "quote(e.x1)||'|'||quote(e.y1)||'|'||quote(e.x2)||'|'||quote(e.y2)"),
        (
            "A",
            "d_arc",
            "quote(e.cx)||'|'||quote(e.cy)||'|'||quote(e.r)||'|'||quote(e.a0)||'|'||quote(e.a1)",
        ),
        (
            "T",
            "d_text",
            "quote(e.x)||'|'||quote(e.y)||'|'||quote(e.height)||'|'||quote(e.rotation)||'|'||quote(e.content)||'|'||quote(e.mtext_width)||'|'||quote(e.attach)",
        ),
        (
            "P",
            "d_polyline",
            "quote(e.closed)||'|'||quote(e.hatch)||'|'||quote(e.id-e.hatch_id)",
        ),
    ];
    for (tag, table, cols) in simple {
        let q = format!(
            "SELECT e.id,l.name||'|'||{cols}||'|'||{sty} FROM {table} e JOIN d_layer l ON l.id=e.layer WHERE e.block=?1 ORDER BY e.id"
        );
        let mut st = db.prepare(&q)?;
        let rows: Vec<(i64, String)> = st
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        for (rid, line) in rows {
            out.push_str(tag);
            out.push_str(&line);
            if table == "d_polyline" {
                let mut vs = db.prepare_cached(
                    "SELECT quote(x)||','||quote(y)||','||quote(bulge) FROM d_vertex WHERE polyline=?1 ORDER BY idx",
                )?;
                for v in vs.query_map([rid], |r| r.get::<_, String>(0))? {
                    out.push(';');
                    out.push_str(&v?);
                }
            }
            out.push('\n');
        }
    }
    let q = format!(
        "SELECT e.id,e.def,l.name||'|'||quote(e.x)||'|'||quote(e.y)||'|'||quote(e.sx)||'|'||quote(e.sy)||'|'||quote(e.rotation)||'|'||{sty} FROM d_insert e JOIN d_layer l ON l.id=e.layer WHERE e.block=?1 ORDER BY e.id"
    );
    let mut st = db.prepare(&q)?;
    let rows: Vec<(i64, i64, String)> = st
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    for (rid, def, line) in rows {
        let name = match names.get(&def) {
            Some(n) => n.clone(),
            None => db.query_row("SELECT name FROM d_block WHERE id=?1", [def], |r| r.get(0))?,
        };
        out.push_str(&format!("I{name}|{line}"));
        let mut at = db.prepare_cached("SELECT tag,value FROM d_attr WHERE insert_id=?1 ORDER BY tag")?;
        for a in at.query_map([rid], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (t, v) = a?;
            out.push_str(&format!(";{t}={v}"));
        }
        out.push('\n');
        targets.push(name);
    }
    let mut st = db.prepare(
        "SELECT tag,default_value,quote(x),quote(y),quote(height) FROM d_attdef WHERE block=?1 ORDER BY tag",
    )?;
    let rows = st.query_map([id], |r| {
        Ok(format!(
            "D{}|{}|{}|{}|{}\n",
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?
        ))
    })?;
    for r in rows {
        out.push_str(&r?);
    }
    Ok((out, targets))
}

/// Provisional blocks (`name~N`, made for names that already exist) are compared with the existing
/// block. A block is identical when its content matches and every block it inserts is identical too;
/// identical ones are dropped (inserts repointed to the existing block), the rest stay renamed.
fn resolve_clashes(cx: &mut Ctx, todo: &[(&Block, i64, Option<i64>)]) -> Result<(), Error> {
    let db = cx.db;
    let mut items: Vec<(String, i64, i64, String, Vec<String>)> = vec![];
    for (b, prov, clash) in todo {
        let Some(old) = clash else { continue };
        let (sp, targets) = block_sig(db, *prov, &cx.owner_names)?;
        let (so, _) = block_sig(db, *old, &cx.owner_names)?;
        let same = sp == so;
        items.push((b.name.clone(), *prov, *old, if same { String::new() } else { "x".into() }, targets));
    }
    let mut diff: HashSet<String> = items
        .iter()
        .filter(|i| !i.3.is_empty())
        .map(|i| i.0.clone())
        .collect();
    loop {
        let mut changed = false;
        for it in &items {
            if !diff.contains(&it.0) && it.4.iter().any(|t| diff.contains(t)) {
                diff.insert(it.0.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for it in &items {
        if diff.contains(&it.0) {
            cx.rep.renamed_blocks += 1;
            continue;
        }
        db.execute("UPDATE d_insert SET def=?2 WHERE def=?1", [it.1, it.2])?;
        cx.blocks.insert(it.0.clone(), it.2);
        cx.owner_names.remove(&it.1);
    }
    for it in &items {
        if diff.contains(&it.0) {
            continue;
        }
        let id = it.1;
        // content of a dropped duplicate was counted while read: take it back out of the report
        let n = |q: &str| -> Result<usize, Error> {
            Ok(db.query_row(q, [id], |r| r.get::<_, i64>(0))? as usize)
        };
        let full = "a0=0 AND a1=6.283185307179586";
        let (lines, texts, inserts) = (
            n("SELECT COUNT(*) FROM d_line WHERE block=?1")?,
            n("SELECT COUNT(*) FROM d_text WHERE block=?1")?,
            n("SELECT COUNT(*) FROM d_insert WHERE block=?1")?,
        );
        let polylines = n("SELECT COUNT(*) FROM d_polyline WHERE block=?1 AND hatch IS NULL")?;
        let circles = n(&format!("SELECT COUNT(*) FROM d_arc WHERE block=?1 AND {full}"))?;
        let arcs = n("SELECT COUNT(*) FROM d_arc WHERE block=?1")?.saturating_sub(circles);
        let attrs = n(
            "SELECT COUNT(*) FROM d_attr WHERE insert_id IN (SELECT id FROM d_insert WHERE block=?1)",
        )?;
        let rep = &mut cx.rep;
        rep.lines = rep.lines.saturating_sub(lines);
        rep.texts = rep.texts.saturating_sub(texts);
        rep.inserts = rep.inserts.saturating_sub(inserts);
        rep.polylines = rep.polylines.saturating_sub(polylines);
        rep.circles = rep.circles.saturating_sub(circles);
        rep.arcs = rep.arcs.saturating_sub(arcs);
        rep.attributes = rep.attributes.saturating_sub(attrs);
        for q in [
            "DELETE FROM d_vertex WHERE polyline IN (SELECT id FROM d_polyline WHERE block=?1)",
            "DELETE FROM d_attr WHERE insert_id IN (SELECT id FROM d_insert WHERE block=?1)",
            "DELETE FROM d_line WHERE block=?1",
            "DELETE FROM d_polyline WHERE block=?1",
            "DELETE FROM d_arc WHERE block=?1",
            "DELETE FROM d_text WHERE block=?1",
            "DELETE FROM d_insert WHERE block=?1",
            "DELETE FROM d_attdef WHERE block=?1",
            "DELETE FROM d_block WHERE id=?1",
        ] {
            db.execute(q, [id])?;
        }
        cx.rep.reused_blocks += 1;
    }
    Ok(())
}

/// Layer from `base`, style from `style`.
fn with_layer(style: Common, base: Common) -> Common {
    Common {
        layer: base.layer,
        ..style
    }
}

fn pt(x: f64, y: f64) -> P3 {
    [x, y, 0.0]
}

/// Contents of `block` (None = model space) as DXF entities.
fn entities(
    db: &Connection,
    block: Option<i64>,
    layers: &HashMap<i64, String>,
) -> Result<Vec<Entity>, Error> {
    let cm = |id: i64| Common::on_layer(layers.get(&id).map_or("0", |s| s.as_str()));
    // Row style: NULL fields stay None, i.e. recorded as "by layer".
    let sty = |c: &mut Common, r: &rusqlite::Row, at: usize| -> rusqlite::Result<()> {
        c.color = r.get::<_, Option<i32>>(at)?;
        c.true_color = r.get::<_, Option<i32>>(at + 1)?;
        c.linetype = r.get(at + 2)?;
        c.lineweight = r.get::<_, Option<i32>>(at + 3)?;
        Ok(())
    };
    let mut out = vec![];
    let mut st =
        db.prepare("SELECT layer,x1,y1,x2,y2,color,true_color,linetype,lineweight FROM d_line WHERE block IS ?1 ORDER BY id")?;
    let rows = st.query_map([block], |r| {
        let mut c = Common::default();
        sty(&mut c, r, 5)?;
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, c))
    })?;
    for r in rows {
        let (l, x1, y1, x2, y2, st): (i64, f64, f64, f64, f64, Common) = r?;
        out.push(Entity::Line(Line {
            common: with_layer(st, cm(l)),
            start: pt(x1, y1),
            end: pt(x2, y2),
        }));
    }
    let mut vs = db.prepare("SELECT x,y,bulge FROM d_vertex WHERE polyline=?1 ORDER BY idx")?;
    let mut st =
        db.prepare("SELECT id,layer,closed,color,true_color,linetype,lineweight,hatch,hatch_id FROM d_polyline WHERE block IS ?1 ORDER BY id")?;
    let rows = st.query_map([block], |r| {
        let mut c = Common::default();
        sty(&mut c, r, 3)?;
        let hatch: Option<(String, i64)> =
            match (r.get::<_, Option<String>>(7)?, r.get::<_, Option<i64>>(8)?) {
                (Some(p), Some(h)) => Some((p, h)),
                _ => None,
            };
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, c, hatch))
    })?;
    // the outlines of one hatch are gathered into one HATCH record (at the place of the first outline)
    let mut hatches: Vec<(i64, Common, String, Vec<Vec<(f64, f64)>>)> = vec![];
    for r in rows {
        let (id, l, closed, st, hatch): (i64, i64, bool, Common, Option<(String, i64)>) = r?;
        if let Some((pattern, hid)) = hatch {
            let ring: Vec<(f64, f64)> = vs
                .query_map([id], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)))?
                .collect::<Result<_, _>>()?;
            match hatches.iter_mut().find(|h| h.0 == hid) {
                Some(h) => h.3.push(ring),
                None => hatches.push((hid, with_layer(st, cm(l)), pattern, vec![ring])),
            }
            continue;
        }
        let vertices = vs
            .query_map([id], |r| {
                Ok(LwVertex {
                    x: r.get(0)?,
                    y: r.get(1)?,
                    bulge: r.get(2)?,
                    ..Default::default()
                })
            })?
            .collect::<Result<_, _>>()?;
        out.push(Entity::LwPolyline(LwPolyline {
            common: with_layer(st, cm(l)),
            flags: closed as i32,
            vertices,
        }));
    }
    for (_, c, pattern, loops) in hatches {
        out.push(Entity::Unknown(super::lower::hatch_record(
            &c, &loops, &pattern,
        )));
    }
    let mut st =
        db.prepare("SELECT layer,cx,cy,r,a0,a1,color,true_color,linetype,lineweight FROM d_arc WHERE block IS ?1 ORDER BY id")?;
    let rows = st.query_map([block], |r| {
        let mut c = Common::default();
        sty(&mut c, r, 6)?;
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            c,
        ))
    })?;
    for r in rows {
        let (l, cx, cy, radius, a0, a1, st): (i64, f64, f64, f64, f64, f64, Common) = r?;
        let (common, center) = (with_layer(st, cm(l)), pt(cx, cy));
        if a0 == 0.0 && a1 == TAU {
            out.push(Entity::Circle(Circle {
                common,
                center,
                radius,
            }));
        } else {
            let (start_angle, end_angle) = (a0.to_degrees(), a1.to_degrees());
            out.push(Entity::Arc(Arc {
                common,
                center,
                radius,
                start_angle,
                end_angle,
            }));
        }
    }
    let mut st = db.prepare(
        "SELECT layer,x,y,height,rotation,content,mtext_width,attach,color,true_color,linetype,lineweight FROM d_text WHERE block IS ?1 ORDER BY id",
    )?;
    type TextRow = (
        i64,
        f64,
        f64,
        f64,
        f64,
        String,
        Option<f64>,
        Option<i32>,
        Common,
    );
    let rows = st.query_map([block], |r| {
        let mut c = Common::default();
        sty(&mut c, r, 8)?;
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
            r.get(7)?,
            c,
        ))
    })?;
    for r in rows {
        let (l, x, y, height, rot, text, mw, attach, st): TextRow = r?;
        let common = with_layer(st, cm(l));
        let (pos, rotation) = (pt(x, y), rot.to_degrees());
        out.push(match mw {
            Some(width) => Entity::MText(MText {
                common,
                pos,
                height,
                width,
                text,
                rotation,
                style: None,
                attach,
            }),
            None => Entity::Text(Text {
                common,
                pos,
                height,
                text,
                rotation,
                style: None,
            }),
        });
    }
    {
        let mut at = db.prepare("SELECT tag,value FROM d_attr WHERE insert_id=?1 ORDER BY tag")?;
        let mut st = db.prepare(
            "SELECT i.id,i.layer,b.name,i.x,i.y,i.sx,i.sy,i.rotation,i.color,i.true_color,i.linetype,i.lineweight FROM d_insert i \
             JOIN d_block b ON b.id=i.def WHERE i.block IS ?1 ORDER BY i.id",
        )?;
        let rows = st.query_map([block], |r| {
            let mut c = Common::default();
            sty(&mut c, r, 8)?;
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                c,
            ))
        })?;
        type InsRow = (i64, i64, String, f64, f64, f64, f64, f64, Common);
        for r in rows {
            let (id, l, name, x, y, sx, sy, rot, st): InsRow = r?;
            let attribs = at
                .query_map([id], |r| {
                    Ok(Attrib {
                        common: cm(l),
                        tag: r.get(0)?,
                        value: r.get(1)?,
                        ..Default::default()
                    })
                })?
                .collect::<Result<_, _>>()?;
            out.push(Entity::Insert(Insert {
                common: with_layer(st, cm(l)),
                block: name,
                pos: pt(x, y),
                scale: [sx, sy, 1.0],
                rotation: rot.to_degrees(),
                attribs,
            }));
        }
    }
    Ok(out)
}

/// Model → DXF document (layers, blocks with attribute definitions, model entities).
#[cfg(any(test, feature = "dxf-write"))]
pub fn export_from(model: &Model) -> Result<DxfDocument, Error> {
    let db = model.sql();
    let mut doc = DxfDocument::default();
    let mut layers = HashMap::new();
    let mut st =
        db.prepare("SELECT id,name,color,linetype,weight,true_color FROM d_layer ORDER BY id")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get::<_, Option<i32>>(5)?,
        ))
    })?;
    for r in rows {
        let (id, name, color, linetype, w, tc): (i64, String, i32, String, f64, Option<i32>) = r?;
        layers.insert(id, name.clone());
        let mut extra = vec![Pair::new(370, snap_lineweight(w).to_string())];
        if let Some(tc) = tc {
            extra.push(Pair::new(420, tc.to_string()));
        }
        doc.layers.push(Layer {
            name,
            flags: 0,
            color,
            linetype: if linetype.trim().is_empty() {
                "Continuous".into()
            } else {
                linetype
            },
            extra,
        });
    }
    let mut st = db.prepare("SELECT name,description,length,pattern FROM d_ltype ORDER BY id")?;
    let defs = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    for d in defs {
        let (name, description, len, pat) = d?;
        let pattern: Vec<f64> = pat
            .split_whitespace()
            .filter_map(|v| v.parse().ok())
            .collect();
        doc.ltypes.push(LineType {
            name,
            description,
            flags: 0,
            pattern_length: len,
            pattern,
            extra: vec![],
        });
    }
    let mut st = db.prepare("SELECT id,name,bx,by FROM d_block ORDER BY id")?;
    let blocks: Vec<(i64, String, f64, f64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    for (id, name, bx, by) in blocks {
        let mut b = Block::new(name);
        b.base = pt(bx, by);
        let mut st = db.prepare(
            "SELECT tag,default_value,x,y,height FROM d_attdef WHERE block=?1 ORDER BY tag",
        )?;
        let rows = st.query_map([id], |r| {
            Ok(AttDef {
                common: Common::default(),
                tag: r.get(0)?,
                prompt: r.get(0)?,
                default: r.get(1)?,
                pos: pt(r.get(2)?, r.get(3)?),
                height: r.get(4)?,
                flags: 0,
            })
        })?;
        b.attdefs = rows.collect::<Result<_, _>>()?;
        b.entities = entities(db, Some(id), &layers)?;
        doc.blocks.push(b);
    }
    doc.entities = entities(db, None, &layers)?;
    // Every linetype name used by a layer or an entity needs a table record.
    let mut used: Vec<String> = doc.layers.iter().map(|l| l.linetype.clone()).collect();
    for t in ["d_line", "d_polyline", "d_arc", "d_text", "d_insert"] {
        let mut st = db.prepare(&format!(
            "SELECT DISTINCT linetype FROM {t} WHERE linetype IS NOT NULL"
        ))?;
        used.extend(st.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?);
    }
    ltypes::complete(&mut doc, used.iter().map(String::as_str));
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lineweight_snaps_to_allowed_set() {
        assert_eq!(snap_lineweight(0.1), 9);
        assert_eq!(snap_lineweight(0.25), 25);
        assert_eq!(snap_lineweight(0.0), 0);
        assert_eq!(snap_lineweight(5.0), 211);
        assert_eq!(snap_lineweight(f64::NAN), -3);
        assert_eq!(snap_lineweight(-1.0), -3);
        for w in [0.01, 0.07, 0.3, 0.45, 0.77, 1.23] {
            assert!(LINEWEIGHTS.contains(&snap_lineweight(w)));
        }
    }
    use web_time::Instant;

    fn layer(n: &str, color: i32, w: i64) -> Layer {
        Layer {
            name: n.into(),
            flags: 0,
            color,
            linetype: "DASHED".into(),
            extra: vec![Pair::new(370, w.to_string())],
        }
    }

    fn count(m: &Model, t: &str) -> i64 {
        m.sql()
            .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
            .unwrap()
    }

    fn near(a: f64, b: f64) {
        assert!((a - b).abs() <= 1e-9, "{a} vs {b}");
    }

    fn sample() -> DxfDocument {
        let mut d = DxfDocument::default();
        d.layers = vec![layer("0", 7, 25), layer("WALLS", 3, 50)];
        let mut b = Block::new("DOOR");
        b.base = [0.5, 0.25, 0.0];
        b.attdefs.push(AttDef {
            tag: "MARK".into(),
            prompt: "MARK".into(),
            default: "D?".into(),
            pos: [0.1, 0.2, 0.0],
            height: 0.2,
            ..Default::default()
        });
        let c0 = Common::on_layer("0");
        b.entities.push(Entity::Line(Line {
            common: c0.clone(),
            start: [0.0; 3],
            end: [0.9, 0.0, 0.0],
        }));
        b.entities.push(Entity::Arc(Arc {
            common: c0,
            center: [0.0; 3],
            radius: 0.9,
            start_angle: 0.0,
            end_angle: 90.0,
        }));
        d.blocks.push(b);
        let w = Common::on_layer("WALLS");
        d.entities.push(Entity::Line(Line {
            common: w.clone(),
            start: [1.0, 2.0, 0.0],
            end: [3.5, 4.25, 0.0],
        }));
        let v = |x, y, bulge| LwVertex {
            x,
            y,
            bulge,
            ..Default::default()
        };
        d.entities.push(Entity::LwPolyline(LwPolyline {
            common: w.clone(),
            flags: 1,
            vertices: vec![
                v(0.0, 0.0, 0.0),
                v(2.0, 0.0, 0.41421356237),
                v(2.0, 2.0, -0.3),
            ],
        }));
        d.entities.push(Entity::Circle(Circle {
            common: w.clone(),
            center: [5.0, 5.0, 0.0],
            radius: 1.5,
        }));
        d.entities.push(Entity::Arc(Arc {
            common: w.clone(),
            center: [1.0, 1.0, 0.0],
            radius: 2.0,
            start_angle: 30.0,
            end_angle: 200.0,
        }));
        d.entities.push(Entity::Text(Text {
            common: w.clone(),
            pos: [1.0, 1.0, 0.0],
            height: 0.25,
            text: "hello".into(),
            rotation: 45.0,
            style: None,
        }));
        d.entities.push(Entity::Insert(Insert {
            common: w,
            block: "DOOR".into(),
            pos: [3.0, 4.0, 0.0],
            scale: [1.0, 2.0, 1.0],
            rotation: 90.0,
            attribs: vec![Attrib {
                tag: "MARK".into(),
                value: "D-12".into(),
                ..Default::default()
            }],
        }));
        d.entities.push(Entity::Insert(Insert {
            block: "NOPE".into(),
            ..Default::default()
        }));
        d.entities
            .push(Entity::Unknown(vec![Pair::new(0, "HATCH")]));
        d
    }

    #[test]
    fn roundtrip_and_single_undo() {
        let doc = sample();
        let mut m = Model::open(":memory:").unwrap();
        let rep = import_into(&mut m, &doc).unwrap();
        assert_eq!((rep.skipped_unknown, rep.skipped_inserts), (1, 1));
        assert_eq!(
            (rep.layers, rep.blocks, rep.inserts, rep.attributes),
            (2, 1, 1, 1)
        );

        let out = export_from(&m).unwrap();
        // A skipped insert of an unknown block and an unknown entity are not written back.
        assert_eq!(out.entities.len(), doc.entities.len() - 2);
        assert_eq!(out.layers.len(), 2);
        for (a, b) in out.layers.iter().zip(&doc.layers) {
            assert_eq!(
                (&a.name, a.color, &a.linetype),
                (&b.name, b.color, &b.linetype)
            );
            near(layer_weight(a), layer_weight(b));
        }
        let (ob, ib) = (&out.blocks[0], &doc.blocks[0]);
        assert_eq!((ob.entities.len(), ob.attdefs.len()), (2, 1));
        assert_eq!(
            (ob.attdefs[0].tag.as_str(), ob.attdefs[0].default.as_str()),
            ("MARK", "D?")
        );
        near(ob.base[0], ib.base[0]);
        let (Entity::Arc(a), Entity::Arc(b)) = (&ob.entities[1], &ib.entities[1]) else {
            panic!()
        };
        near(a.end_angle, b.end_angle);

        // Output order: lines, polylines, arcs/circles, texts, inserts.
        let o = &out.entities;
        let (Entity::Line(a), Entity::Line(b)) = (&o[0], &doc.entities[0]) else {
            panic!()
        };
        (0..3).for_each(|i| {
            near(a.start[i], b.start[i]);
            near(a.end[i], b.end[i]);
        });
        let (Entity::LwPolyline(a), Entity::LwPolyline(b)) = (&o[1], &doc.entities[1]) else {
            panic!()
        };
        assert!(a.closed());
        for (u, v) in a.vertices.iter().zip(&b.vertices) {
            near(u.x, v.x);
            near(u.y, v.y);
            near(u.bulge, v.bulge);
        }
        let (Entity::Arc(a), Entity::Arc(b)) = (&o[3], &doc.entities[3]) else {
            panic!()
        };
        near(a.start_angle, b.start_angle);
        near(a.end_angle, b.end_angle);
        near(a.radius, b.radius);
        let (Entity::Circle(a), Entity::Circle(b)) = (&o[2], &doc.entities[2]) else {
            panic!()
        };
        near(a.radius, b.radius);
        near(a.center[0], b.center[0]);
        let (Entity::Text(a), Entity::Text(b)) = (&o[4], &doc.entities[4]) else {
            panic!()
        };
        assert_eq!(a.text, b.text);
        near(a.rotation, b.rotation);
        let (Entity::Insert(a), Entity::Insert(b)) = (&o[5], &doc.entities[5]) else {
            panic!()
        };
        assert_eq!(
            (a.block.as_str(), a.attribs[0].value.as_str()),
            ("DOOR", "D-12")
        );
        near(a.rotation, b.rotation);
        near(a.scale[1], 2.0);

        m.undo().unwrap();
        for t in [
            "d_layer",
            "d_ltype",
            "d_block",
            "d_attdef",
            "d_line",
            "d_polyline",
            "d_vertex",
            "d_arc",
            "d_text",
            "d_insert",
            "d_attr",
        ] {
            assert_eq!(count(&m, t), 0, "{t}");
        }
        assert!(matches!(m.undo(), Err(Error::NothingToUndo)));
    }

    #[test]
    fn undo_keeps_earlier_data() {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_layer("keep", 1, "CONTINUOUS", 0.1).unwrap();
        m.add_line(l, None, (0., 0.), (1., 1.)).unwrap();
        import_into(&mut m, &sample()).unwrap();
        m.undo().unwrap();
        assert_eq!((count(&m, "d_layer"), count(&m, "d_line")), (1, 1));
    }

    #[test]
    fn import_100k_lines_timing() {
        let mut d = DxfDocument::default();
        d.layers.push(layer("0", 7, 25));
        for i in 0..100_000 {
            let x = i as f64;
            d.entities.push(Entity::Line(Line {
                common: Common::on_layer("0"),
                start: [x, 0.0, 0.0],
                end: [x, 10.0, 0.0],
            }));
        }
        let mut m = Model::open(":memory:").unwrap();
        let t = Instant::now();
        let rep = import_into(&mut m, &d).unwrap();
        let s = t.elapsed().as_secs_f64();
        println!("import 100k lines: {s:.3} s");
        assert_eq!(rep.lines, 100_000);
        assert!(s < if cfg!(debug_assertions) { 15.0 } else { 1.0 }, "{s}");
        m.undo().unwrap();
        assert_eq!(count(&m, "d_line"), 0);
    }

    fn styled() -> Common {
        Common {
            color: Some(1),
            true_color: Some(0x00FF8800),
            linetype: Some("DASHED".into()),
            lineweight: Some(50),
            ..Common::on_layer("WALLS")
        }
    }

    #[test]
    fn mtext_and_entity_style_roundtrip_through_model() {
        let mut d = DxfDocument::default();
        d.entities.push(Entity::MText(MText {
            common: styled(),
            pos: [1.0, 2.0, 0.0],
            height: 2.5,
            width: 40.0,
            text: r"first\Psecond".into(),
            rotation: 0.0,
            style: None,
            attach: Some(5),
        }));
        d.entities.push(Entity::Text(Text {
            common: Common::on_layer("WALLS"),
            pos: [0.0; 3],
            height: 1.0,
            text: "plain".into(),
            rotation: 0.0,
            style: None,
        }));
        d.entities.push(Entity::Line(Line {
            common: styled(),
            start: [0.0; 3],
            end: [1.0, 0.0, 0.0],
        }));
        d.entities.push(Entity::Arc(Arc {
            common: styled(),
            center: [0.0; 3],
            radius: 1.0,
            start_angle: 0.0,
            end_angle: 90.0,
        }));
        d.entities.push(Entity::Circle(Circle {
            common: styled(),
            center: [0.0; 3],
            radius: 1.0,
        }));
        let mut m = Model::open(":memory:").unwrap();
        import_into(&mut m, &d).unwrap();
        let back = export_from(&m).unwrap();
        // Via DXF text: style and MTEXT survive writing and reading.
        let back = crate::dxf::read_str(&crate::dxf::write_string(&back)).unwrap();
        let mt = back
            .entities
            .iter()
            .find_map(|e| {
                if let Entity::MText(t) = e {
                    Some(t)
                } else {
                    None
                }
            })
            .expect("MTEXT stays MTEXT");
        assert_eq!((mt.width, mt.attach), (40.0, Some(5)));
        assert_eq!(mt.text, r"first\Psecond");
        assert_eq!(mt.common.color, Some(1));
        assert_eq!(mt.common.true_color, Some(0x00FF8800));
        assert_eq!(mt.common.linetype.as_deref(), Some("DASHED"));
        assert_eq!(mt.common.lineweight, Some(50));
        let plain: Vec<_> = back
            .entities
            .iter()
            .filter(|e| matches!(e, Entity::Text(_)))
            .collect();
        assert_eq!(plain.len(), 1);
        let Entity::Text(p) = plain[0] else { panic!() };
        assert_eq!((p.common.color, p.common.lineweight), (None, None));
        for e in &back.entities {
            let c = match e {
                Entity::Line(x) => &x.common,
                Entity::Arc(x) => &x.common,
                Entity::Circle(x) => &x.common,
                _ => continue,
            };
            assert_eq!(c.color, Some(1), "{e:?}");
            assert_eq!(c.linetype.as_deref(), Some("DASHED"));
            assert_eq!(c.lineweight, Some(50));
            assert_eq!(c.true_color, Some(0x00FF8800));
        }
    }

    #[test]
    fn style_api_sets_reads_and_undoes() {
        use ddc_model::{Entity as Ke, Style};
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_layer("A", 7, "CONTINUOUS", 0.25).unwrap();
        let id = m.add_line(l, None, (0., 0.), (1., 0.)).unwrap();
        assert_eq!(m.style(Ke::Line, id).unwrap(), Style::default());
        let st = Style {
            color: Some(3),
            true_color: None,
            linetype: Some("HIDDEN".into()),
            lineweight: Some(35),
        };
        m.set_style(Ke::Line, id, &st).unwrap();
        assert_eq!(m.style(Ke::Line, id).unwrap(), st);
        m.undo().unwrap();
        assert_eq!(m.style(Ke::Line, id).unwrap(), Style::default());
    }

    fn circle(r: f64) -> Entity {
        Entity::Circle(Circle {
            common: Common::on_layer("0"),
            center: [0.0; 3],
            radius: r,
        })
    }

    fn arc_ent(r: f64) -> Entity {
        Entity::Arc(Arc {
            common: Common::on_layer("0"),
            center: [0.0; 3],
            radius: r,
            start_angle: 0.0,
            end_angle: 90.0,
        })
    }

    fn line_ent() -> Entity {
        Entity::Line(Line {
            common: Common::on_layer("0"),
            start: [0.0; 3],
            end: [1.0, 0.0, 0.0],
        })
    }

    #[test]
    fn import_into_skips_zero_radius_circle() {
        let mut d = DxfDocument::default();
        d.entities = vec![circle(0.0), circle(f64::NAN), circle(2.0)];
        let mut m = Model::open(":memory:").unwrap();
        import_into(&mut m, &d).unwrap();
        assert_eq!(count(&m, "d_arc"), 1);
    }

    #[test]
    fn import_into_skips_zero_radius_arc_keeps_rest() {
        let mut d = DxfDocument::default();
        d.entities = vec![line_ent(), arc_ent(0.0), arc_ent(-1.0), arc_ent(1.5), line_ent()];
        let mut m = Model::open(":memory:").unwrap();
        let rep = import_into(&mut m, &d).unwrap();
        assert_eq!(count(&m, "d_line"), 2);
        assert_eq!(count(&m, "d_arc"), 1);
        assert_eq!(rep.arcs, 1);
    }

    #[test]
    fn import_report_counts_degenerate_circles() {
        let mut d = DxfDocument::default();
        d.entities = vec![circle(0.0), circle(0.0), arc_ent(0.0), circle(1.0)];
        let mut m = Model::open(":memory:").unwrap();
        let rep = import_into(&mut m, &d).unwrap();
        assert_eq!(rep.circles, 1);
        assert_eq!(rep.skipped_unknown, 3);
        assert_eq!(rep.dropped.values().sum::<usize>(), 3);
    }

    fn nan_text(x: f64, h: f64) -> Entity {
        Entity::Text(Text {
            common: Common::on_layer("0"),
            pos: [x, 0.0, 0.0],
            height: h,
            text: "t".into(),
            rotation: 0.0,
            style: None,
        })
    }

    #[test]
    fn import_skips_line_with_nan_coordinate() {
        let mut d = DxfDocument::default();
        let bad = Entity::Line(Line {
            common: Common::on_layer("0"),
            start: [f64::NAN, 0.0, 0.0],
            end: [1.0, f64::INFINITY, 0.0],
        });
        d.entities = vec![line_ent(), bad, line_ent()];
        let mut m = Model::open(":memory:").unwrap();
        let rep = import_into(&mut m, &d).unwrap();
        assert_eq!(count(&m, "d_line"), 2);
        assert_eq!(rep.dropped.values().sum::<usize>(), 1);
    }

    #[test]
    fn import_skips_arc_with_nonfinite_center_or_angle() {
        let mut d = DxfDocument::default();
        let mut a = arc_ent(1.0);
        if let Entity::Arc(x) = &mut a {
            x.center[0] = f64::NAN;
        }
        let mut b = arc_ent(1.0);
        if let Entity::Arc(x) = &mut b {
            x.end_angle = f64::INFINITY;
        }
        let mut c = circle(1.0);
        if let Entity::Circle(x) = &mut c {
            x.center[1] = f64::NEG_INFINITY;
        }
        d.entities = vec![a, b, c, arc_ent(1.0)];
        let mut m = Model::open(":memory:").unwrap();
        let rep = import_into(&mut m, &d).unwrap();
        assert_eq!(count(&m, "d_arc"), 1);
        assert_eq!(rep.dropped.values().sum::<usize>(), 3);
    }

    #[test]
    fn import_skips_text_with_nonfinite_position_or_height() {
        let mut d = DxfDocument::default();
        d.entities = vec![nan_text(f64::NAN, 1.0), nan_text(0.0, f64::INFINITY), nan_text(0.0, 1.0)];
        let mut m = Model::open(":memory:").unwrap();
        let rep = import_into(&mut m, &d).unwrap();
        assert_eq!(count(&m, "d_text"), 1);
        assert_eq!(rep.dropped.values().sum::<usize>(), 2);
    }

    #[test]
    fn import_skips_polyline_with_nonfinite_vertex() {
        let v = |x: f64| LwVertex { x, y: 0.0, bulge: 0.0, start_width: 0.0, end_width: 0.0 };
        let mut d = DxfDocument::default();
        d.entities = vec![
            Entity::LwPolyline(LwPolyline {
                common: Common::on_layer("0"),
                flags: 0,
                vertices: vec![v(0.0), v(f64::NAN)],
            }),
            line_ent(),
        ];
        let mut m = Model::open(":memory:").unwrap();
        import_into(&mut m, &d).unwrap();
        assert_eq!(count(&m, "d_polyline"), 0);
        assert_eq!(count(&m, "d_line"), 1);
    }

    fn roundtrip_text(d: &DxfDocument) -> DxfDocument {
        let mut m = Model::open(":memory:").unwrap();
        import_into(&mut m, d).unwrap();
        let out = export_from(&m).unwrap();
        crate::dxf::read_str(&crate::dxf::write_string(&out)).unwrap()
    }

    fn lt<'a>(d: &'a DxfDocument, n: &str) -> Option<&'a LineType> {
        d.ltypes.iter().find(|t| t.name.eq_ignore_ascii_case(n))
    }

    fn doc_with_line(layer_lt: &str, ent_lt: Option<&str>) -> DxfDocument {
        let mut d = DxfDocument::default();
        let mut l = layer("0", 7, 25);
        l.linetype = layer_lt.into();
        d.layers = vec![l];
        let mut c = Common::on_layer("0");
        c.linetype = ent_lt.map(String::from);
        d.entities.push(Entity::Line(Line {
            common: c,
            start: [0.0; 3],
            end: [1.0, 0.0, 0.0],
        }));
        d
    }

    #[test]
    fn export_defines_ltype_for_dashed_layer() {
        let back = roundtrip_text(&doc_with_line("DASHED", None));
        let t = lt(&back, "DASHED").expect("DASHED record");
        assert_eq!(t.pattern, vec![0.5, -0.25]);
        assert!((t.pattern_length - 0.75).abs() < 1e-9);
        assert!(lt(&back, "Continuous").is_some());
    }

    #[test]
    fn every_used_linetype_has_table_record() {
        let back = roundtrip_text(&doc_with_line("HIDDEN", Some("CENTER")));
        for n in ["HIDDEN", "CENTER", "ByLayer", "ByBlock", "Continuous"] {
            assert!(lt(&back, n).is_some(), "{n}");
        }
        for l in &back.layers {
            assert!(lt(&back, &l.linetype).is_some(), "{}", l.linetype);
        }
        let Entity::Line(e) = &back.entities[0] else {
            panic!()
        };
        assert!(lt(&back, e.common.linetype.as_deref().unwrap()).is_some());
    }

    #[test]
    fn unknown_linetype_name_falls_back_to_solid_record() {
        let back = roundtrip_text(&doc_with_line("MY_SPECIAL", Some("OTHER_ONE")));
        for n in ["MY_SPECIAL", "OTHER_ONE"] {
            let t = lt(&back, n).expect(n);
            assert!(t.pattern.is_empty());
        }
    }

    #[test]
    fn source_linetype_definition_is_kept() {
        let mut d = doc_with_line("SITE_DASH", None);
        d.ltypes.push(LineType {
            name: "SITE_DASH".into(),
            description: "site".into(),
            flags: 0,
            pattern_length: 2.0,
            pattern: vec![1.5, -0.5],
            extra: vec![],
        });
        let t = roundtrip_text(&d).ltypes.iter().find(|t| t.name == "SITE_DASH").cloned().unwrap();
        assert_eq!(t.pattern, vec![1.5, -0.5]);
        assert_eq!(t.description, "site");
    }

    #[test]
    fn layer_true_color_dxf_roundtrip() {
        let mut d = doc_with_line("DASHED", None);
        d.layers[0].extra.push(Pair::new(420, 0x00FF8800.to_string()));
        let back = roundtrip_text(&d);
        assert_eq!(layer_true_color(&back.layers[0]), Some(0x00FF8800));
        let mut m = Model::open(":memory:").unwrap();
        import_into(&mut m, &doc_with_line("DASHED", None)).unwrap();
        let out = export_from(&m).unwrap();
        assert_eq!(layer_true_color(&out.layers[0]), None);
    }

    fn line_block(name: &str, len: f64) -> Block {
        let mut b = Block::new(name);
        b.entities.push(Entity::Line(Line {
            common: Common::on_layer("0"),
            start: [0.0; 3],
            end: [len, 0.0, 0.0],
        }));
        b
    }

    fn ins(block: &str, x: f64) -> Entity {
        Entity::Insert(Insert {
            common: Common::on_layer("0"),
            block: block.into(),
            pos: [x, 0.0, 0.0],
            scale: [1.0; 3],
            rotation: 0.0,
            attribs: vec![],
        })
    }

    fn doc_blocks(blocks: Vec<Block>, ents: Vec<Entity>) -> DxfDocument {
        let mut d = DxfDocument::default();
        d.layers = vec![layer("0", 7, 25)];
        d.blocks = blocks;
        d.entities = ents;
        d
    }

    fn block_len(m: &Model, name: &str) -> f64 {
        m.sql()
            .query_row(
                "SELECT x2 FROM d_line l JOIN d_block b ON b.id=l.block WHERE b.name=?1",
                [name],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn nested_insert_survives_import_and_dxf_export() {
        let mut set = Block::new("SET");
        set.entities.push(ins("FRAME", 5.0));
        let d = doc_blocks(vec![line_block("FRAME", 1.0), set], vec![ins("SET", 0.0)]);
        let mut m = Model::open(":memory:").unwrap();
        let rep = import_into(&mut m, &d).unwrap();
        assert_eq!((rep.blocks, rep.inserts, rep.cyclic_inserts), (2, 2, 0));
        let nested: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM d_insert WHERE block IS NOT NULL", [], |r| r.get(0))
            .unwrap();
        assert_eq!(nested, 1);
        let out = export_from(&m).unwrap();
        let back = crate::dxf::read_str(&crate::dxf::write_string(&out)).unwrap();
        let set = back.blocks.iter().find(|b| b.name == "SET").unwrap();
        let inner: Vec<_> = set
            .entities
            .iter()
            .filter_map(|e| match e {
                Entity::Insert(i) => Some((i.block.as_str(), i.pos[0])),
                _ => None,
            })
            .collect();
        assert_eq!(inner, vec![("FRAME", 5.0)]);
        let top = back
            .entities
            .iter()
            .filter(|e| matches!(e, Entity::Insert(_)))
            .count();
        assert_eq!(top, 1);
        m.undo().unwrap();
        assert_eq!((count(&m, "d_insert"), count(&m, "d_block")), (0, 0));
    }

    #[test]
    fn same_name_block_other_content_gets_new_name() {
        let mut m = Model::open(":memory:").unwrap();
        import_into(&mut m, &doc_blocks(vec![line_block("Door", 1.0)], vec![ins("Door", 0.0)]))
            .unwrap();
        let rep = import_into(
            &mut m,
            &doc_blocks(vec![line_block("Door", 2.0)], vec![ins("Door", 3.0)]),
        )
        .unwrap();
        assert_eq!((rep.blocks, rep.renamed_blocks, rep.reused_blocks), (1, 1, 0));
        assert_eq!(count(&m, "d_block"), 2);
        near(block_len(&m, "Door"), 1.0);
        near(block_len(&m, "Door~2"), 2.0);
        let target: String = m
            .sql()
            .query_row(
                "SELECT b.name FROM d_insert i JOIN d_block b ON b.id=i.def WHERE i.x=3.0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target, "Door~2");
        // a third file with yet another Door takes the next free suffix
        import_into(&mut m, &doc_blocks(vec![line_block("Door", 4.0)], vec![])).unwrap();
        near(block_len(&m, "Door~3"), 4.0);
    }

    #[test]
    fn reused_block_not_counted() {
        let mut m = Model::open(":memory:").unwrap();
        let d = doc_blocks(vec![line_block("Door", 1.0)], vec![ins("Door", 0.0)]);
        let first = import_into(&mut m, &d).unwrap();
        assert_eq!((first.lines, first.inserts), (1, 1));
        let rep = import_into(&mut m, &d).unwrap();
        assert_eq!(rep.reused_blocks, 1);
        assert_eq!((rep.lines, rep.inserts), (0, 1));
        assert_eq!((count(&m, "d_line"), count(&m, "d_insert")), (1, 2));
    }

    #[test]
    fn same_name_identical_block_reused() {
        let mut m = Model::open(":memory:").unwrap();
        let d = doc_blocks(vec![line_block("Door", 1.0)], vec![ins("Door", 0.0)]);
        import_into(&mut m, &d).unwrap();
        let rep = import_into(&mut m, &d).unwrap();
        assert_eq!((rep.blocks, rep.renamed_blocks, rep.reused_blocks), (0, 0, 1));
        assert_eq!((count(&m, "d_block"), count(&m, "d_line")), (1, 1));
        assert_eq!(count(&m, "d_insert"), 2);
        let defs: i64 = m
            .sql()
            .query_row("SELECT count(DISTINCT def) FROM d_insert", [], |r| r.get(0))
            .unwrap();
        assert_eq!(defs, 1);
        m.undo().unwrap();
        assert_eq!((count(&m, "d_block"), count(&m, "d_insert")), (1, 1));
    }

    #[test]
    fn cyclic_block_reference_refused_not_hang() {
        let mut a = Block::new("A");
        a.entities.push(ins("B", 0.0));
        a.entities.push(ins("A", 1.0));
        let mut b = Block::new("B");
        b.entities.push(ins("A", 0.0));
        b.entities.push(Entity::Line(Line {
            common: Common::on_layer("0"),
            start: [0.0; 3],
            end: [1.0, 0.0, 0.0],
        }));
        let d = doc_blocks(vec![a, b, line_block("C", 1.0)], vec![ins("A", 0.0)]);
        let mut m = Model::open(":memory:").unwrap();
        let rep = import_into(&mut m, &d).unwrap();
        assert_eq!(rep.cyclic_inserts, 3);
        assert_eq!(count(&m, "d_block"), 3);
        // only the model-space insert remains
        assert_eq!(count(&m, "d_insert"), 1);
        assert!(export_from(&m).is_ok());
    }
}
