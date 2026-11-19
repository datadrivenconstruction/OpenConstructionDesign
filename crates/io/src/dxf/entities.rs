//! Entity types and their group-code mapping.

#[cfg(any(test, feature = "dxf-write"))]
use super::writer::Out;
use super::{DxfError, Pair};

pub type Pt = [f64; 3];

/// Attributes shared by all entities. Unrecognised pairs land in `extra`.
#[derive(Debug, Clone, PartialEq)]
pub struct Common {
    pub layer: String,
    pub linetype: Option<String>,
    pub color: Option<i32>,
    /// Code 420: 24-bit RGB.
    pub true_color: Option<i32>,
    /// Code 370: line weight in 1/100 mm (negative = by layer/block/default).
    pub lineweight: Option<i32>,
    pub extra: Vec<Pair>,
}

impl Default for Common {
    fn default() -> Self {
        Common {
            layer: "0".into(),
            linetype: None,
            color: None,
            true_color: None,
            lineweight: None,
            extra: vec![],
        }
    }
}

impl Common {
    pub fn on_layer(layer: &str) -> Self {
        Common {
            layer: layer.into(),
            ..Default::default()
        }
    }

    /// Consumes layer/linetype/color and subclass markers.
    fn take(&mut self, p: &Pair) -> Result<bool, DxfError> {
        match p.code {
            8 => self.layer = p.value.clone(),
            6 => self.linetype = Some(p.value.clone()),
            62 => self.color = Some(int(p)?),
            420 => self.true_color = Some(int(p)?),
            370 => self.lineweight = Some(int(p)?),
            c if is_structural(c) => {}
            _ => return Ok(false),
        }
        Ok(true)
    }

    #[cfg(any(test, feature = "dxf-write"))]
    fn write(&self, w: &mut Out) {
        w.s(100, "AcDbEntity");
        w.s(8, &self.layer);
        if let Some(l) = &self.linetype {
            w.s(6, l);
        }
        if let Some(c) = self.color {
            w.i(62, c);
        }
        if let Some(c) = self.true_color {
            w.i(420, c);
        }
        if let Some(c) = self.lineweight {
            w.i(370, c);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Line {
    pub common: Common,
    pub start: Pt,
    pub end: Pt,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct LwVertex {
    pub x: f64,
    pub y: f64,
    pub bulge: f64,
    pub start_width: f64,
    pub end_width: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct LwPolyline {
    pub common: Common,
    /// Code 70 flags; bit 1 = closed.
    pub flags: i32,
    pub vertices: Vec<LwVertex>,
}

impl LwPolyline {
    pub fn closed(&self) -> bool {
        self.flags & 1 != 0
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Circle {
    pub common: Common,
    pub center: Pt,
    pub radius: f64,
}

/// Angles in degrees, counter-clockwise.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Arc {
    pub common: Common,
    pub center: Pt,
    pub radius: f64,
    pub start_angle: f64,
    pub end_angle: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Text {
    pub common: Common,
    pub pos: Pt,
    pub height: f64,
    pub text: String,
    pub rotation: f64,
    pub style: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MText {
    pub common: Common,
    pub pos: Pt,
    pub height: f64,
    pub width: f64,
    pub text: String,
    pub rotation: f64,
    pub style: Option<String>,
    /// Code 71: attachment point 1..9.
    pub attach: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AttDef {
    pub common: Common,
    pub tag: String,
    pub prompt: String,
    pub default: String,
    pub pos: Pt,
    pub height: f64,
    pub flags: i32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Attrib {
    pub common: Common,
    pub tag: String,
    pub value: String,
    pub pos: Pt,
    pub height: f64,
    pub flags: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Insert {
    pub common: Common,
    pub block: String,
    pub pos: Pt,
    pub scale: Pt,
    pub rotation: f64,
    pub attribs: Vec<Attrib>,
}

impl Default for Insert {
    fn default() -> Self {
        Insert {
            common: Common::default(),
            block: String::new(),
            pos: [0.0; 3],
            scale: [1.0; 3],
            rotation: 0.0,
            attribs: vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Entity {
    Line(Line),
    LwPolyline(LwPolyline),
    Circle(Circle),
    Arc(Arc),
    Text(Text),
    MText(MText),
    Insert(Insert),
    /// Only meaningful inside a block; the reader moves these into `Block::attdefs`.
    AttDef(AttDef),
    /// Verbatim record, first pair is code 0 with the entity type.
    Unknown(Vec<Pair>),
}

/// Pairs the writer regenerates (handles, owners, extension groups, subclass markers); dropped on read.
pub(super) fn is_structural(code: i32) -> bool {
    matches!(code, 5 | 100 | 102 | 105 | 330 | 347 | 348 | 360 | 390)
}

fn num(p: &Pair) -> Result<f64, DxfError> {
    p.value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| DxfError(format!("bad number for code {}: {:?}", p.code, p.value)))
}

fn int(p: &Pair) -> Result<i32, DxfError> {
    p.value
        .trim()
        .parse::<i32>()
        .map_err(|_| DxfError(format!("bad integer for code {}: {:?}", p.code, p.value)))
}

/// Point component: codes base, base+10, base+20 are x, y, z.
fn pt(dst: &mut Pt, base: i32, p: &Pair) -> Result<bool, DxfError> {
    match p.code - base {
        0 => dst[0] = num(p)?,
        10 => dst[1] = num(p)?,
        20 => dst[2] = num(p)?,
        _ => return Ok(false),
    }
    Ok(true)
}

#[cfg(any(test, feature = "dxf-write"))]
fn write_pt(w: &mut Out, base: i32, v: &Pt) {
    w.f(base, v[0]);
    w.f(base + 10, v[1]);
    w.f(base + 20, v[2]);
}

pub(super) fn parse_entity(rec: &[Pair]) -> Result<Entity, DxfError> {
    let body = &rec[1..];
    Ok(match rec[0].value.as_str() {
        "LINE" => {
            let mut e = Line::default();
            for p in body {
                if e.common.take(p)? || pt(&mut e.start, 10, p)? || pt(&mut e.end, 11, p)? {
                    continue;
                }
                e.common.extra.push(p.clone());
            }
            Entity::Line(e)
        }
        "LWPOLYLINE" => {
            let mut e = LwPolyline::default();
            for p in body {
                if e.common.take(p)? {
                    continue;
                }
                match (p.code, e.vertices.last_mut()) {
                    (10, _) => e.vertices.push(LwVertex {
                        x: num(p)?,
                        ..Default::default()
                    }),
                    (20, Some(v)) => v.y = num(p)?,
                    (40, Some(v)) => v.start_width = num(p)?,
                    (41, Some(v)) => v.end_width = num(p)?,
                    (42, Some(v)) => v.bulge = num(p)?,
                    (70, _) => e.flags = int(p)?,
                    (90, _) => {}
                    _ => e.common.extra.push(p.clone()),
                }
            }
            Entity::LwPolyline(e)
        }
        "CIRCLE" => {
            let mut e = Circle::default();
            for p in body {
                if e.common.take(p)? || pt(&mut e.center, 10, p)? {
                    continue;
                }
                if p.code == 40 {
                    e.radius = num(p)?;
                } else {
                    e.common.extra.push(p.clone());
                }
            }
            Entity::Circle(e)
        }
        "ARC" => {
            let mut e = Arc::default();
            for p in body {
                if e.common.take(p)? || pt(&mut e.center, 10, p)? {
                    continue;
                }
                match p.code {
                    40 => e.radius = num(p)?,
                    50 => e.start_angle = num(p)?,
                    51 => e.end_angle = num(p)?,
                    _ => e.common.extra.push(p.clone()),
                }
            }
            Entity::Arc(e)
        }
        "TEXT" => {
            let mut e = Text::default();
            for p in body {
                if e.common.take(p)? || pt(&mut e.pos, 10, p)? {
                    continue;
                }
                match p.code {
                    40 => e.height = num(p)?,
                    1 => e.text = p.value.clone(),
                    50 => e.rotation = num(p)?,
                    7 => e.style = Some(p.value.clone()),
                    _ => e.common.extra.push(p.clone()),
                }
            }
            Entity::Text(e)
        }
        "MTEXT" => {
            let mut e = MText::default();
            let mut chunks = String::new();
            for p in body {
                if e.common.take(p)? || pt(&mut e.pos, 10, p)? {
                    continue;
                }
                match p.code {
                    40 => e.height = num(p)?,
                    41 => e.width = num(p)?,
                    71 => e.attach = Some(int(p)?),
                    3 => chunks.push_str(&p.value),
                    1 => {
                        chunks.push_str(&p.value);
                        e.text = std::mem::take(&mut chunks);
                    }
                    50 => e.rotation = num(p)?,
                    7 => e.style = Some(p.value.clone()),
                    _ => e.common.extra.push(p.clone()),
                }
            }
            if !chunks.is_empty() {
                e.text = chunks;
            }
            Entity::MText(e)
        }
        "ATTDEF" => {
            let mut e = AttDef::default();
            for p in body {
                if e.common.take(p)? || pt(&mut e.pos, 10, p)? {
                    continue;
                }
                match p.code {
                    2 => e.tag = p.value.clone(),
                    3 => e.prompt = p.value.clone(),
                    1 => e.default = p.value.clone(),
                    40 => e.height = num(p)?,
                    70 => e.flags = int(p)?,
                    _ => e.common.extra.push(p.clone()),
                }
            }
            Entity::AttDef(e)
        }
        "INSERT" => {
            let mut e = Insert::default();
            for p in body {
                if e.common.take(p)? || pt(&mut e.pos, 10, p)? {
                    continue;
                }
                match p.code {
                    2 => e.block = p.value.clone(),
                    41 => e.scale[0] = num(p)?,
                    42 => e.scale[1] = num(p)?,
                    43 => e.scale[2] = num(p)?,
                    50 => e.rotation = num(p)?,
                    66 => {}
                    _ => e.common.extra.push(p.clone()),
                }
            }
            Entity::Insert(e)
        }
        _ => Entity::Unknown(rec.to_vec()),
    })
}

fn parse_attrib(rec: &[Pair]) -> Result<Attrib, DxfError> {
    let mut e = Attrib::default();
    for p in &rec[1..] {
        if e.common.take(p)? || pt(&mut e.pos, 10, p)? {
            continue;
        }
        match p.code {
            2 => e.tag = p.value.clone(),
            1 => e.value = p.value.clone(),
            40 => e.height = num(p)?,
            70 => e.flags = int(p)?,
            _ => e.common.extra.push(p.clone()),
        }
    }
    Ok(e)
}

/// Parses records of one entity list; ATTRIB/SEQEND following an INSERT are folded into it.
pub(super) fn parse_all(records: &[&[Pair]]) -> Result<Vec<Entity>, DxfError> {
    let mut out: Vec<Entity> = vec![];
    let mut in_attribs = false;
    for rec in records {
        match rec[0].value.as_str() {
            "ATTRIB" if in_attribs => {
                if let Some(Entity::Insert(ins)) = out.last_mut() {
                    ins.attribs.push(parse_attrib(rec)?);
                }
            }
            "SEQEND" if in_attribs => in_attribs = false,
            _ => {
                let e = parse_entity(rec)?;
                in_attribs = matches!(e, Entity::Insert(_))
                    && rec.iter().any(|p| p.code == 66 && p.value.trim() != "0");
                out.push(e);
            }
        }
    }
    Ok(out)
}

#[cfg(any(test, feature = "dxf-write"))]
pub(super) fn write_entity(w: &mut Out, e: &Entity) {
    match e {
        Entity::Line(e) => {
            w.rec("LINE");
            e.common.write(w);
            w.s(100, "AcDbLine");
            write_pt(w, 10, &e.start);
            write_pt(w, 11, &e.end);
            w.extra_m(&e.common.extra);
        }
        Entity::LwPolyline(e) => {
            w.rec("LWPOLYLINE");
            e.common.write(w);
            w.s(100, "AcDbPolyline");
            w.i(90, e.vertices.len() as i32);
            w.i(70, e.flags);
            for v in &e.vertices {
                w.f(10, v.x);
                w.f(20, v.y);
                if v.start_width != 0.0 || v.end_width != 0.0 {
                    w.f(40, v.start_width);
                    w.f(41, v.end_width);
                }
                if v.bulge != 0.0 {
                    w.f(42, v.bulge);
                }
            }
            w.extra_m(&e.common.extra);
        }
        Entity::Circle(e) => {
            w.rec("CIRCLE");
            e.common.write(w);
            w.s(100, "AcDbCircle");
            write_pt(w, 10, &e.center);
            w.f(40, e.radius);
            w.extra_m(&e.common.extra);
        }
        Entity::Arc(e) => {
            w.rec("ARC");
            e.common.write(w);
            w.s(100, "AcDbCircle");
            write_pt(w, 10, &e.center);
            w.f(40, e.radius);
            w.s(100, "AcDbArc");
            w.f(50, e.start_angle);
            w.f(51, e.end_angle);
            w.extra_m(&e.common.extra);
        }
        Entity::Text(e) => {
            w.rec("TEXT");
            e.common.write(w);
            w.s(100, "AcDbText");
            write_pt(w, 10, &e.pos);
            w.f(40, e.height);
            w.s(1, &e.text);
            w.f(50, e.rotation);
            if let Some(s) = &e.style {
                w.s(7, s);
            }
            w.extra_m(&e.common.extra);
        }
        Entity::MText(e) => {
            w.rec("MTEXT");
            e.common.write(w);
            w.s(100, "AcDbMText");
            write_pt(w, 10, &e.pos);
            w.f(40, e.height);
            w.f(41, e.width);
            if let Some(a) = e.attach {
                w.i(71, a);
            }
            // Line breaks are `\P` in MTEXT; raw newlines would break the group pairs.
            let text = e.text.replace("\r\n", "\\P").replace(['\n', '\r'], "\\P");
            // Long text is split into 250-char pieces: code 3 for all but the last, code 1 for the last.
            let chars: Vec<char> = text.chars().collect();
            let mut pieces: Vec<String> = chars.chunks(250).map(|c| c.iter().collect()).collect();
            let last = pieces.pop().unwrap_or_default();
            for p in &pieces {
                w.s(3, p);
            }
            w.s(1, &last);
            w.f(50, e.rotation);
            if let Some(s) = &e.style {
                w.s(7, s);
            }
            w.extra_m(&e.common.extra);
        }
        Entity::AttDef(e) => write_attdef(w, e),
        Entity::Insert(e) => {
            let ins_h = w.rec("INSERT");
            e.common.write(w);
            w.s(100, "AcDbBlockReference");
            if !e.attribs.is_empty() {
                w.i(66, 1);
            }
            w.s(2, &e.block);
            write_pt(w, 10, &e.pos);
            w.f(41, e.scale[0]);
            w.f(42, e.scale[1]);
            w.f(43, e.scale[2]);
            w.f(50, e.rotation);
            w.extra_m(&e.common.extra);
            if !e.attribs.is_empty() {
                let model_owner = std::mem::replace(&mut w.owner, ins_h);
                for a in &e.attribs {
                    w.rec("ATTRIB");
                    a.common.write(w);
                    w.s(100, "AcDbText");
                    write_pt(w, 10, &a.pos);
                    w.f(40, a.height);
                    w.s(1, &a.value);
                    w.s(100, "AcDbAttribute");
                    w.s(2, &a.tag);
                    w.i(70, a.flags);
                    w.extra_m(&a.common.extra);
                }
                w.rec("SEQEND");
                w.s(8, &e.common.layer);
                w.owner = model_owner;
            }
        }
        Entity::Unknown(pairs) => w.unknown(pairs),
    }
}

#[cfg(any(test, feature = "dxf-write"))]
pub(super) fn write_attdef(w: &mut Out, e: &AttDef) {
    w.rec("ATTDEF");
    e.common.write(w);
    w.s(100, "AcDbText");
    write_pt(w, 10, &e.pos);
    w.f(40, e.height);
    w.s(1, &e.default);
    w.s(100, "AcDbAttributeDefinition");
    w.s(3, &e.prompt);
    w.s(2, &e.tag);
    w.i(70, e.flags);
    w.extra_m(&e.common.extra);
}
