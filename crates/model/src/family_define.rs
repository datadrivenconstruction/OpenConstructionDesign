// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Families defined by a program as structured JSON (`FamilyOp::Define`): JSON is read into the
//! typed `FamilyDef`, validated and flex-checked by `geom::component::define`, then stored as a
//! draft family (its text carries `status draft`) until a person promotes it.
//!
//! ```json
//! {"name": "AiTable", "category": "furniture", "host": "none",
//!  "params": [{"name": "Width", "type": "length", "scope": "type", "unit": "mm",
//!              "default": 1600, "min": 400, "max": 3000},
//!             {"name": "Pitch", "type": "length", "formula": "Width / (Legs - 2)"}],
//!  "refs": [{"name": "Mid", "axis": "x", "at": "0"}, {"name": "Left", "axis": "x", "free": "-500mm"}],
//!  "constraints": [{"eq": ["Left", "Mid", "Right"]}, {"dim": "Width", "a": "Left", "b": "Right"}],
//!  "forms": [{"name": "Board", "extrude": {"rect": ["Left", "Front", "Right", "Back"]},
//!             "z": ["Height - Top", "Height"], "mat": "Finish"}],
//!  "types": [{"name": "1600x800", "values": {"Width": 1600}}],
//!  "tests": [{"Width": 900}]}
//! ```
//! Other forms: `{"revolve": profile, "axis": [x, y], "angle": a}`, `{"sweep": profile,
//! "path": [[x, y, z], ...]}`, `{"blend": [profile, profile], "z": [a, b]}`; profiles are
//! `{"rect": [x0, y0, x1, y1]}`, `{"circle": [cx, cy, r]}`, `{"poly": [[x, y], ...]}`.

use ddc_geom::component::define::{
    self, ConsDef, FamilyDef, FormDef, FormShape, Lit, ParamDef, ProfileDef, RefAt, RefDef,
    Rejection, SweepRow, TypeDef,
};
use ddc_geom::component::{Axis, Category, Family, Hosting, Kind};
use rusqlite::OptionalExtension;
use serde_json::{Map, Value as J};

use crate::{Error, Model};

fn rej(path: &str, msg: impl Into<String>) -> Rejection {
    Rejection {
        path: path.into(),
        code: "schema",
        msg: msg.into(),
        test: None,
    }
}

fn obj<'a>(v: &'a J, path: &str) -> Result<&'a Map<String, J>, Rejection> {
    v.as_object().ok_or_else(|| rej(path, "expected an object"))
}

fn arr<'a>(v: Option<&'a J>, path: &str) -> Result<&'a [J], Rejection> {
    match v {
        None | Some(J::Null) => Ok(&[]),
        Some(J::Array(a)) => Ok(a),
        Some(_) => Err(rej(path, "expected an array")),
    }
}

fn text(v: Option<&J>, path: &str) -> Result<String, Rejection> {
    v.and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| rej(path, "expected a string"))
}

fn opt_text(v: Option<&J>, path: &str) -> Result<Option<String>, Rejection> {
    match v {
        None | Some(J::Null) => Ok(None),
        Some(_) => text(v, path).map(Some),
    }
}

/// Expression: a string, or a number taken as written.
fn expr(v: Option<&J>, path: &str) -> Result<String, Rejection> {
    match v {
        Some(J::String(s)) => Ok(s.clone()),
        Some(J::Number(n)) => Ok(n.to_string()),
        _ => Err(rej(path, "expected an expression (string or number)")),
    }
}

fn exprs<const N: usize>(v: Option<&J>, path: &str) -> Result<[String; N], Rejection> {
    let a = arr(v, path)?;
    if a.len() != N {
        return Err(rej(path, format!("expected {N} values")));
    }
    let mut out: [String; N] = std::array::from_fn(|_| String::new());
    for (k, x) in a.iter().enumerate() {
        out[k] = expr(Some(x), &format!("{path}/{k}"))?;
    }
    Ok(out)
}

fn points<const N: usize>(v: Option<&J>, path: &str) -> Result<Vec<[String; N]>, Rejection> {
    arr(v, path)?
        .iter()
        .enumerate()
        .map(|(k, p)| exprs::<N>(Some(p), &format!("{path}/{k}")))
        .collect()
}

fn opt_num(v: Option<&J>, path: &str) -> Result<Option<f64>, Rejection> {
    match v {
        None | Some(J::Null) => Ok(None),
        Some(J::Number(n)) => Ok(n.as_f64()),
        Some(_) => Err(rej(path, "expected a number")),
    }
}

fn lit(v: &J, path: &str) -> Result<Lit, Rejection> {
    match v {
        J::Number(n) => n
            .as_f64()
            .map(Lit::Num)
            .ok_or_else(|| rej(path, "number out of range")),
        J::Bool(b) => Ok(Lit::Num(if *b { 1.0 } else { 0.0 })),
        J::String(s) => Ok(Lit::Text(s.clone())),
        _ => Err(rej(path, "expected a number, boolean or string")),
    }
}

fn values(v: Option<&J>, path: &str) -> Result<Vec<(String, Lit)>, Rejection> {
    let Some(v) = v else { return Ok(vec![]) };
    obj(v, path)?
        .iter()
        .map(|(k, x)| Ok((k.clone(), lit(x, &format!("{path}/{k}"))?)))
        .collect()
}

fn axis(v: Option<&J>, path: &str, default: Option<Axis>) -> Result<Axis, Rejection> {
    match (v.and_then(|v| v.as_str()), default) {
        (Some("x"), _) => Ok(Axis::X),
        (Some("y"), _) => Ok(Axis::Y),
        (Some("z"), _) => Ok(Axis::Z),
        (None, Some(d)) if v.is_none() => Ok(d),
        _ => Err(rej(path, "axis must be x, y or z")),
    }
}

fn profile(v: &J, path: &str) -> Result<ProfileDef, Rejection> {
    let o = obj(v, path)?;
    if let Some(r) = o.get("rect") {
        return Ok(ProfileDef::Rect(exprs::<4>(Some(r), &format!("{path}/rect"))?));
    }
    if let Some(c) = o.get("circle") {
        return Ok(ProfileDef::Circle(exprs::<3>(Some(c), &format!("{path}/circle"))?));
    }
    if let Some(p) = o.get("poly") {
        return Ok(ProfileDef::Poly(points::<2>(Some(p), &format!("{path}/poly"))?));
    }
    Err(rej(path, "profile must be rect, circle or poly"))
}

fn param(v: &J, path: &str) -> Result<ParamDef, Rejection> {
    let o = obj(v, path)?;
    let kind = text(o.get("type"), &format!("{path}/type"))?;
    let kind = Kind::parse(&kind).ok_or_else(|| rej(&format!("{path}/type"), format!("unknown type {kind}")))?;
    let instance = match o.get("scope").and_then(|s| s.as_str()) {
        None | Some("type") => false,
        Some("instance") => true,
        Some(_) => return Err(rej(&format!("{path}/scope"), "scope must be type or instance")),
    };
    let default = match o.get("default") {
        None | Some(J::Null) => None,
        Some(d) => Some(lit(d, &format!("{path}/default"))?),
    };
    Ok(ParamDef {
        name: text(o.get("name"), &format!("{path}/name"))?,
        kind,
        instance,
        unit: opt_text(o.get("unit"), &format!("{path}/unit"))?,
        default,
        formula: opt_text(o.get("formula"), &format!("{path}/formula"))?,
        min: opt_num(o.get("min"), &format!("{path}/min"))?,
        max: opt_num(o.get("max"), &format!("{path}/max"))?,
    })
}

fn reference(v: &J, path: &str) -> Result<RefDef, Rejection> {
    let o = obj(v, path)?;
    let at = match (o.get("at"), o.get("free")) {
        (Some(a), None) => RefAt::Formula(expr(Some(a), &format!("{path}/at"))?),
        (None, Some(g)) => RefAt::Free(expr(Some(g), &format!("{path}/free"))?),
        _ => return Err(rej(path, "a plane needs exactly one of at or free")),
    };
    Ok(RefDef {
        name: text(o.get("name"), &format!("{path}/name"))?,
        axis: axis(o.get("axis"), &format!("{path}/axis"), None)?,
        at,
    })
}

fn constraint(v: &J, path: &str) -> Result<ConsDef, Rejection> {
    let o = obj(v, path)?;
    let ab = || -> Result<(String, String), Rejection> {
        Ok((text(o.get("a"), &format!("{path}/a"))?, text(o.get("b"), &format!("{path}/b"))?))
    };
    let names = |k: &str| -> Result<Vec<String>, Rejection> {
        arr(o.get(k), &format!("{path}/{k}"))?
            .iter()
            .enumerate()
            .map(|(i, x)| text(Some(x), &format!("{path}/{k}/{i}")))
            .collect()
    };
    if let Some(d) = o.get("dim") {
        let (a, b) = ab()?;
        return Ok(ConsDef::Dim { value: expr(Some(d), &format!("{path}/dim"))?, a, b });
    }
    if let Some(d) = o.get("angle") {
        let (a, b) = ab()?;
        return Ok(ConsDef::Angle { value: expr(Some(d), &format!("{path}/angle"))?, a, b });
    }
    if o.contains_key("eq") {
        return Ok(ConsDef::Eq(names("eq")?));
    }
    if o.contains_key("align") {
        let n = names("align")?;
        if let [a, b] = n.as_slice() {
            return Ok(ConsDef::Align(a.clone(), b.clone()));
        }
        return Err(rej(&format!("{path}/align"), "align needs 2 planes"));
    }
    Err(rej(path, "constraint must be dim, angle, eq or align"))
}

fn form(v: &J, path: &str) -> Result<FormDef, Rejection> {
    let o = obj(v, path)?;
    let p = |k: &str| format!("{path}/{k}");
    let z = || -> Result<(String, String), Rejection> {
        let [a, b] = exprs::<2>(o.get("z"), &p("z"))?;
        Ok((a, b))
    };
    let shape = if let Some(pr) = o.get("extrude") {
        let (from, to) = z()?;
        FormShape::Extrude {
            profile: profile(pr, &p("extrude"))?,
            dir: axis(o.get("dir"), &p("dir"), Some(Axis::Z))?,
            from,
            to,
        }
    } else if let Some(pr) = o.get("revolve") {
        FormShape::Revolve {
            profile: profile(pr, &p("revolve"))?,
            axis: match o.get("axis") {
                None => [String::from("0"), String::from("0")],
                a => exprs::<2>(a, &p("axis"))?,
            },
            angle: match o.get("angle") {
                None => "360deg".into(),
                a => expr(a, &p("angle"))?,
            },
        }
    } else if let Some(pr) = o.get("sweep") {
        FormShape::Sweep {
            profile: profile(pr, &p("sweep"))?,
            path: points::<3>(o.get("path"), &p("path"))?,
        }
    } else if let Some(pr) = o.get("blend") {
        let a = arr(Some(pr), &p("blend"))?;
        if a.len() != 2 {
            return Err(rej(&p("blend"), "blend needs 2 profiles"));
        }
        let (from, to) = z()?;
        FormShape::Blend {
            bottom: profile(&a[0], &p("blend/0"))?,
            top: profile(&a[1], &p("blend/1"))?,
            from,
            to,
        }
    } else {
        return Err(rej(path, "form must be extrude, revolve, sweep or blend"));
    };
    Ok(FormDef {
        name: text(o.get("name"), &p("name"))?,
        shape,
        void: o.get("void").and_then(|v| v.as_bool()).unwrap_or(false),
        material: opt_text(o.get("mat"), &p("mat"))?,
    })
}

fn list<T>(o: &Map<String, J>, k: &str, f: fn(&J, &str) -> Result<T, Rejection>) -> Result<Vec<T>, Rejection> {
    arr(o.get(k), &format!("/{k}"))?
        .iter()
        .enumerate()
        .map(|(i, x)| f(x, &format!("/{k}/{i}")))
        .collect()
}

/// Reads the structured JSON of `FamilyOp::Define`; paths in refusals point into it.
pub fn family_def_from_json(src: &str) -> Result<FamilyDef, Rejection> {
    let v: J = serde_json::from_str(src).map_err(|e| rej("", format!("JSON: {e}")))?;
    let o = obj(&v, "")?;
    let category = match o.get("category") {
        None => Category::Generic,
        c => {
            let c = text(c, "/category")?;
            Category::parse(&c).ok_or_else(|| rej("/category", format!("unknown category {c}")))?
        }
    };
    let hosting = match o.get("host") {
        None => Hosting::None,
        h => {
            let h = text(h, "/host")?;
            Hosting::parse(&h).ok_or_else(|| rej("/host", format!("unknown host {h}")))?
        }
    };
    let types = list(o, "types", |t, path| {
        let t = obj(t, path)?;
        Ok(TypeDef {
            name: text(t.get("name"), &format!("{path}/name"))?,
            values: values(t.get("values"), &format!("{path}/values"))?,
        })
    })?;
    Ok(FamilyDef {
        name: text(o.get("name"), "/name")?,
        category,
        hosting,
        params: list(o, "params", param)?,
        refs: list(o, "refs", reference)?,
        constraints: list(o, "constraints", constraint)?,
        forms: list(o, "forms", form)?,
        types,
        tests: list(o, "tests", |t, path| values(Some(t), path))?,
    })
}

/// Accepted draft: family id and where each limited parameter's sweep breaks.
pub type Defined = Result<(i64, Vec<SweepRow>), Rejection>;

impl Model {
    /// Validates, flex-checks and stores a program-defined family as a draft (one undo step).
    /// A refusal is returned as `Ok(Err(..))`; `Err` is a storage failure.
    pub fn define_family(&mut self, d: &FamilyDef) -> Result<Defined, Error> {
        let a = match define::define(d) {
            Ok(a) => a,
            Err(r) => return Ok(Err(r)),
        };
        if self.family_id(&a.family.name)?.is_some() {
            return Ok(Err(Rejection {
                path: "/name".into(),
                code: "exists",
                msg: format!("family {} already exists", a.family.name),
                test: None,
            }));
        }
        let id = self.load_family(&a.text)?;
        Ok(Ok((id, a.sweeps)))
    }

    pub fn define_family_json(&mut self, json: &str) -> Result<Defined, Error> {
        match family_def_from_json(json) {
            Ok(d) => self.define_family(&d),
            Err(r) => Ok(Err(r)),
        }
    }

    pub fn family_is_draft(&self, id: i64) -> Result<bool, Error> {
        Ok(self.family(id)?.draft)
    }

    /// A person accepts a draft family: `status draft` is dropped from its text (one undo step).
    pub fn promote_family(&mut self, id: i64) -> Result<(), Error> {
        let def: String = self
            .db
            .query_row("SELECT def FROM comp_family WHERE id=?1", [id], |r| r.get(0))
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no component family {id}")))?;
        let f = Family::parse(&def).map_err(|e| Error::Invalid(format!("component: {e}")))?;
        if !f.draft {
            return Err(Error::Invalid(format!("family {} is not a draft", f.name)));
        }
        self.reload_family(id, &define::promoted(&def))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = r#"{"name": "AiDesk", "category": "furniture",
      "params": [
        {"name": "Width", "type": "length", "unit": "mm", "default": 1200, "min": 400, "max": 3000},
        {"name": "Depth", "type": "length", "unit": "mm", "default": 600},
        {"name": "Height", "type": "length", "unit": "mm", "default": 740},
        {"name": "Legs", "type": "int", "default": 4, "min": 1, "max": 8},
        {"name": "Pitch", "type": "length", "formula": "Width / (Legs - 2)"}],
      "refs": [{"name": "Mid", "axis": "x", "at": 0},
               {"name": "Left", "axis": "x", "free": "-500mm"},
               {"name": "Right", "axis": "x", "free": "500mm"}],
      "constraints": [{"eq": ["Left", "Mid", "Right"]}, {"dim": "Width", "a": "Left", "b": "Right"}],
      "forms": [{"name": "Board", "extrude": {"rect": ["Left", 0, "Right", "Depth"]},
                 "z": ["Height - 40mm", "Height"]}],
      "types": [{"name": "Std", "values": {"Width": 1400}}],
      "tests": [{"Width": 800}, {"Legs": 6}]}"#;

    #[test]
    fn json_define_is_draft_until_promoted() {
        let mut m = Model::open(":memory:").unwrap();
        let (id, sweeps) = m.define_family_json(TABLE).unwrap().unwrap();
        assert!(m.family_is_draft(id).unwrap());
        assert!(sweeps.iter().any(|s| s.param == "Legs" && s.breaks_at.is_some()));
        assert_eq!(m.comp_types(id).unwrap().len(), 1);
        m.promote_family(id).unwrap();
        assert!(!m.family_is_draft(id).unwrap());
        assert!(m.promote_family(id).is_err());
        m.undo().unwrap();
        assert!(m.family_is_draft(id).unwrap());
        // same name again is refused, not overwritten
        let r = m.define_family_json(TABLE).unwrap().unwrap_err();
        assert_eq!((r.code, r.path.as_str()), ("exists", "/name"));
    }

    #[test]
    fn json_refusals_point_into_the_definition() {
        let mut m = Model::open(":memory:").unwrap();
        let bad = TABLE.replace(r#"{"Legs": 6}"#, r#"{"Width": 5000}"#);
        let r = m.define_family_json(&bad).unwrap().unwrap_err();
        assert_eq!((r.code, r.path.as_str(), r.test), ("out_of_range", "/params/0", Some(1)));
        assert!(r.msg.contains("Width"), "{r}");
        let r = m.define_family_json(r#"{"name": "X", "params": [{"name": "W"}]}"#).unwrap().unwrap_err();
        assert_eq!(r.path, "/params/0/type");
        assert!(m.families().unwrap().is_empty());
    }
}
