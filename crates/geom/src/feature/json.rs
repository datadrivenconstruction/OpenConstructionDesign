//! JSON form of a timeline (`"k":"timeline"`), shared by the mass forms and the component
//! definitions that carry a timeline.

use serde_json::{json, Value};

use super::{codec, Kind, Node, NodeId, Param, PlaneKind, Shape, Timeline};
use crate::csg::BoolOp;
use crate::sketch_dims::{Dim, DimKind, ProfileSketch, Rule, Seg};

fn bool_name(op: BoolOp) -> &'static str {
    match op {
        BoolOp::Union => "union",
        BoolOp::Subtract => "subtract",
        BoolOp::Intersect => "intersect",
    }
}

fn shape_json(s: &Shape) -> Value {
    match s {
        Shape::Rect { cx, cy, w, h } => json!({"s":"rect","cx":cx,"cy":cy,"w":w,"h":h}),
        Shape::Circle { cx, cy, r, seg } => json!({"s":"circle","cx":cx,"cy":cy,"r":r,"seg":seg}),
        Shape::Poly(p) => json!({"s":"poly","pts":p.iter().map(|q| [&q.0, &q.1]).collect::<Vec<_>>()}),
        Shape::Solved(ps) => json!({
            "s": "solved",
            "pts": ps.pts,
            "segs": ps.segs.iter().map(|s| json!([s.a, s.b, s.construction])).collect::<Vec<_>>(),
            "rules": ps.rules.iter().map(|r| {
                let (k, v) = codec::encode(&r.geo);
                json!([r.tag, k, v])
            }).collect::<Vec<_>>(),
            "dims": ps.dims.iter().map(|d| json!({
                "tag": d.tag, "a": d.a, "b": d.b, "kind": d.kind.key(), "text": d.text,
                "value": d.value, "sign": d.sign, "driven": d.driven, "forced": d.forced,
            })).collect::<Vec<_>>(),
        }),
    }
}

fn kind_json(k: &Kind) -> Value {
    match k {
        Kind::Sketch {
            plane,
            offset,
            shape,
        } => json!({"t":"sketch","plane":plane.name(),"offset":offset,"shape":shape_json(shape)}),
        Kind::Extrude { sketch, dist, start } => {
            json!({"t":"extrude","sketch":sketch,"dist":dist,"start":start})
        }
        Kind::Revolve { sketch, angle, seg } => {
            json!({"t":"revolve","sketch":sketch,"angle":angle,"seg":seg})
        }
        Kind::Sweep { sketch, path } => json!({"t":"sweep","sketch":sketch,"path":path}),
        Kind::Loft { sketches } => json!({"t":"loft","sketches":sketches}),
        Kind::Boolean { op, a, b } => json!({"t":"boolean","op":bool_name(*op),"a":a,"b":b}),
        Kind::Move { input, d, turn } => json!({"t":"move","input":input,"d":d,"turn":turn}),
        Kind::Modify {
            op,
            input,
            faces,
            args,
        } => json!({"t":"modify","op":op,"input":input,"faces":faces,"args":args}),
    }
}

pub fn to_json(t: &Timeline) -> Value {
    json!({
        "k": "timeline",
        "v": 1,
        "params": t.params.iter().map(|p| [&p.name, &p.expr]).collect::<Vec<_>>(),
        "nodes": t.nodes.iter().map(|n| {
            let mut o = json!({"id": n.id, "name": n.name, "off": n.suppressed, "def": kind_json(&n.kind)});
            if !n.keys.is_empty() {
                o["keys"] = json!(n.keys);
            }
            o
        }).collect::<Vec<_>>(),
        "marker": t.marker,
        "next": t.next_id,
    })
}

struct Rd<'a>(&'a Value);

impl<'a> Rd<'a> {
    fn bad(&self) -> String {
        format!("timeline json: {}", self.0)
    }
    fn get(&self, k: &str) -> Result<&'a Value, String> {
        self.0.get(k).ok_or_else(|| self.bad())
    }
    fn s(&self, k: &str) -> Result<String, String> {
        self.get(k)?.as_str().map(str::to_string).ok_or_else(|| self.bad())
    }
    fn u(&self, k: &str) -> Result<u64, String> {
        self.get(k)?.as_u64().ok_or_else(|| self.bad())
    }
    fn id(&self, k: &str) -> Result<NodeId, String> {
        u32::try_from(self.u(k)?).map_err(|_| self.bad())
    }
    fn arr(&self, k: &str) -> Result<&'a Vec<Value>, String> {
        self.get(k)?.as_array().ok_or_else(|| self.bad())
    }
    fn strs(&self, v: &Value, n: usize) -> Result<Vec<String>, String> {
        let a = v.as_array().filter(|a| a.len() == n).ok_or_else(|| self.bad())?;
        a.iter()
            .map(|x| x.as_str().map(str::to_string).ok_or_else(|| self.bad()))
            .collect()
    }
    fn ids(&self, k: &str) -> Result<Vec<u32>, String> {
        self.arr(k)?
            .iter()
            .map(|x| x.as_u64().and_then(|x| u32::try_from(x).ok()).ok_or_else(|| self.bad()))
            .collect()
    }
}

fn shape_from(v: &Value) -> Result<Shape, String> {
    let r = Rd(v);
    Ok(match r.s("s")?.as_str() {
        "rect" => Shape::Rect {
            cx: r.s("cx")?,
            cy: r.s("cy")?,
            w: r.s("w")?,
            h: r.s("h")?,
        },
        "circle" => Shape::Circle {
            cx: r.s("cx")?,
            cy: r.s("cy")?,
            r: r.s("r")?,
            seg: r.u("seg")? as usize,
        },
        "poly" => Shape::Poly(
            r.arr("pts")?
                .iter()
                .map(|p| r.strs(p, 2).map(|s| (s[0].clone(), s[1].clone())))
                .collect::<Result<_, _>>()?,
        ),
        "solved" => {
            let num = |x: &Value| x.as_f64().ok_or_else(|| r.bad());
            let idx = |x: &Value| x.as_u64().map(|x| x as usize).ok_or_else(|| r.bad());
            let pts = r
                .arr("pts")?
                .iter()
                .map(|p| {
                    let a = p.as_array().filter(|a| a.len() == 2).ok_or_else(|| r.bad())?;
                    Ok([num(&a[0])?, num(&a[1])?])
                })
                .collect::<Result<Vec<_>, String>>()?;
            let segs = r
                .arr("segs")?
                .iter()
                .map(|s| {
                    let a = s.as_array().filter(|a| a.len() == 3).ok_or_else(|| r.bad())?;
                    Ok(Seg {
                        a: idx(&a[0])?,
                        b: idx(&a[1])?,
                        construction: a[2].as_bool().ok_or_else(|| r.bad())?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            let rules = r
                .arr("rules")?
                .iter()
                .map(|x| {
                    let a = x.as_array().filter(|a| a.len() == 3).ok_or_else(|| r.bad())?;
                    let nums = a[2]
                        .as_array()
                        .ok_or_else(|| r.bad())?
                        .iter()
                        .map(num)
                        .collect::<Result<Vec<_>, _>>()?;
                    let geo = codec::decode(a[1].as_str().ok_or_else(|| r.bad())?, &nums)
                        .ok_or_else(|| r.bad())?;
                    Ok(Rule {
                        tag: a[0].as_i64().ok_or_else(|| r.bad())?,
                        geo,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            let dims = r
                .arr("dims")?
                .iter()
                .map(|d| {
                    let q = Rd(d);
                    let kind = match q.s("kind")?.as_str() {
                        "aligned" => DimKind::Aligned,
                        "horizontal" => DimKind::Horizontal,
                        "vertical" => DimKind::Vertical,
                        _ => return Err(q.bad()),
                    };
                    let b = |k: &str| q.get(k)?.as_bool().ok_or_else(|| q.bad());
                    Ok(Dim {
                        tag: q.get("tag")?.as_i64().ok_or_else(|| q.bad())?,
                        a: q.u("a")? as usize,
                        b: q.u("b")? as usize,
                        kind,
                        text: q.s("text")?,
                        value: num(q.get("value")?)?,
                        sign: num(q.get("sign")?)?,
                        driven: b("driven")?,
                        forced: b("forced")?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            Shape::Solved(ProfileSketch::from_parts(pts, segs, rules, dims))
        }
        _ => return Err(r.bad()),
    })
}

fn kind_from(v: &Value) -> Result<Kind, String> {
    let r = Rd(v);
    Ok(match r.s("t")?.as_str() {
        "sketch" => Kind::Sketch {
            plane: PlaneKind::parse(&r.s("plane")?).ok_or_else(|| r.bad())?,
            offset: r.s("offset")?,
            shape: shape_from(r.get("shape")?)?,
        },
        "extrude" => Kind::Extrude {
            sketch: r.id("sketch")?,
            dist: r.s("dist")?,
            start: r.s("start")?,
        },
        "revolve" => Kind::Revolve {
            sketch: r.id("sketch")?,
            angle: r.s("angle")?,
            seg: r.u("seg")? as usize,
        },
        "sweep" => Kind::Sweep {
            sketch: r.id("sketch")?,
            path: r
                .arr("path")?
                .iter()
                .map(|p| {
                    let s = r.strs(p, 3)?;
                    Ok([s[0].clone(), s[1].clone(), s[2].clone()])
                })
                .collect::<Result<_, String>>()?,
        },
        "loft" => Kind::Loft {
            sketches: r.ids("sketches")?,
        },
        "boolean" => Kind::Boolean {
            op: match r.s("op")?.as_str() {
                "union" => BoolOp::Union,
                "subtract" => BoolOp::Subtract,
                "intersect" => BoolOp::Intersect,
                _ => return Err(r.bad()),
            },
            a: r.id("a")?,
            b: r.id("b")?,
        },
        "move" => {
            let d = r.strs(r.get("d")?, 3)?;
            Kind::Move {
                input: r.id("input")?,
                d: [d[0].clone(), d[1].clone(), d[2].clone()],
                turn: r.s("turn")?,
            }
        }
        "modify" => Kind::Modify {
            op: r.s("op")?,
            input: r.id("input")?,
            faces: r.ids("faces")?,
            args: r
                .arr("args")?
                .iter()
                .map(|a| r.strs(a, 2).map(|s| (s[0].clone(), s[1].clone())))
                .collect::<Result<_, _>>()?,
        },
        _ => return Err(r.bad()),
    })
}

/// Reads a timeline and checks it (structure, parameter values).
pub fn from_json(v: &Value) -> Result<Timeline, String> {
    let r = Rd(v);
    let params = r
        .arr("params")?
        .iter()
        .map(|p| {
            r.strs(p, 2).map(|s| Param {
                name: s[0].clone(),
                expr: s[1].clone(),
            })
        })
        .collect::<Result<_, _>>()?;
    let nodes = r
        .arr("nodes")?
        .iter()
        .map(|n| {
            let q = Rd(n);
            Ok(Node {
                id: q.id("id")?,
                name: q.s("name")?,
                suppressed: q.get("off")?.as_bool().ok_or_else(|| q.bad())?,
                kind: kind_from(q.get("def")?)?,
                keys: match n.get("keys") {
                    None => vec![],
                    Some(k) => serde_json::from_value(k.clone()).map_err(|_| q.bad())?,
                },
            })
        })
        .collect::<Result<_, String>>()?;
    let marker = match r.get("marker")? {
        Value::Null => None,
        m => Some(m.as_u64().and_then(|x| u32::try_from(x).ok()).ok_or_else(|| r.bad())?),
    };
    let t = Timeline {
        params,
        nodes,
        marker,
        next_id: r.id("next")?,
    };
    t.check().map_err(|e| format!("timeline: {e}"))?;
    t.values().map_err(|e| format!("timeline: {e}"))?;
    Ok(t)
}

/// Compact text of a timeline for one line of a component definition: deflated JSON in
/// URL-safe base64 (no spaces, no parentheses).
pub fn to_data(t: &Timeline) -> String {
    use base64::Engine;
    let raw = to_json(t).to_string();
    let z = miniz_oxide::deflate::compress_to_vec_zlib(raw.as_bytes(), 9);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(z)
}

pub fn from_data(s: &str) -> Result<Timeline, String> {
    use base64::Engine;
    let z = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim())
        .map_err(|e| format!("timeline data: {e}"))?;
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&z, 16 << 20)
        .map_err(|e| format!("timeline data: inflate {:?}", e.status))?;
    let v: Value = serde_json::from_slice(&raw).map_err(|e| format!("timeline data: {e}"))?;
    from_json(&v)
}
