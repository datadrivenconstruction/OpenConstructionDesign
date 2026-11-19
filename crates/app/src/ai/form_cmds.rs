//! Handlers of `measure_save`, `edit_mass` and `mass_to_family`: the saved measurements of the
//! measure tools, one history step of the Forms tab (`mass_tools.rs`) and the history panel's
//! timeline to family (`elem/history.rs`). Each adds one plan item (`modify_plan`); faces and
//! pins resolve on the copy, so Apply replays the preview. Lengths are metres.

use super::modify_cmds::add_op;
use super::modify_plan::{levels, ToolAct};
use super::plan::PlanOp;
use super::session::{Core, Done};
use ddc_ai::registry::{CmdError, ErrorCode};
use ddc_ai::CallCx;
use ddc_model::{
    DimRef, Error, FaceClass, Form, MassFace, MeasureAnchor as A, MeasureKind, Model, PushGoal,
    ViewKey,
};
use serde_json::Value as J;

pub type P3 = (f64, f64, f64);

/// A point closer than this to a face's plane lies on it.
const FACE_TOL: f64 = 0.01;

fn bad(path: &str, why: &str) -> String {
    format!("args.{path}: {why}")
}

fn p3(v: &J) -> Option<P3> {
    let a = v.as_array().filter(|a| a.len() == 3)?;
    let n = |i: usize| a[i].as_f64().filter(|x| x.is_finite());
    Some((n(0)?, n(1)?, n(2)?))
}

fn opt_num(a: &J, k: &str) -> Result<Option<f64>, String> {
    match &a[k] {
        J::Null => Ok(None),
        v => v.as_f64().filter(|x| x.is_finite()).map(Some).ok_or_else(|| bad(k, "needs a number")),
    }
}

/// `mass:ID`, checked against the model.
fn mass_arg(c: &Core, a: &J) -> Result<i64, String> {
    let s = a["mass"].as_str().map(str::trim).unwrap_or_default();
    let id = s.strip_prefix("mass:").and_then(|n| n.trim().parse::<i64>().ok());
    let id = id.ok_or_else(|| bad("mass", "needs mass:ID"))?;
    c.ed.model.mass(id).map_err(|_| {
        CmdError::wire(ErrorCode::NotFound, format!("no mass {id}; query_elements finds them"))
    })?;
    Ok(id)
}

// ---- measure_save -------------------------------------------------------------------------

/// Pins a kind takes, as the model checks them.
fn arity_ok(kind: MeasureKind, n: usize) -> bool {
    match kind {
        MeasureKind::Distance | MeasureKind::Clear | MeasureKind::Slope => n == 2,
        MeasureKind::Angle => n == 2 || n == 3,
        MeasureKind::Area | MeasureKind::Perimeter => n == 1 || n >= 3,
        MeasureKind::Volume => n == 1,
    }
}

/// A pin: a point, or a feature `cat:ID` with an optional `@feature`.
fn pin_of(m: &Model, v: &J) -> Result<A, String> {
    if v.is_array() {
        let p = p3(v).ok_or("needs [x, y, z] in metres")?;
        return Ok(A::Point([p.0, p.1, p.2]));
    }
    let s = v.as_str().map(str::trim).ok_or("needs [x, y, z] or a reference such as wall:3@face0")?;
    let (head, feat) = match s.split_once('@') {
        Some((h, f)) => (h.trim(), Some(f.trim())),
        None => (s, None),
    };
    let (cat, id) = head.split_once(':').ok_or_else(|| format!("{s}: needs cat:ID"))?;
    let id: i64 = id.trim().parse().map_err(|_| format!("{s}: the id is a whole number"))?;
    Ok(match (cat.trim(), feat) {
        ("level", None) => A::Level { id },
        ("room", None) => A::Room { id },
        ("wall", None) | ("mass", None) => A::ElemBox { kind: cat.trim().to_string(), id, corner: 0 },
        ("floor" | "slab", None) => A::ElemBox { kind: "slab".into(), id, corner: 0 },
        ("wall", Some(f @ ("face0" | "face1"))) => {
            A::Dim(DimRef::WallEnd { wall: id, face: u8::from(f == "face1"), end: 0 })
        }
        ("wall", Some("axis")) => A::Dim(DimRef::AxisLine { wall: id }),
        ("wall", Some(f @ ("top" | "bottom"))) => A::Dim(DimRef::WallLevel { wall: id, top: f == "top" }),
        ("floor" | "slab", Some(f @ ("top" | "bottom"))) => {
            A::SlabFace { kind: "slab".into(), id, top: f == "top" }
        }
        ("mass", Some(f)) => {
            let sel = match f.parse::<u32>() {
                Ok(t) => FaceSel::Tag(t),
                Err(_) => FaceSel::Side(side(f).ok_or_else(|| format!("{s}: unknown side"))?),
            };
            let tag = face_tag(m, id, &sel)?;
            m.mass_face_anchor(id, tag).map_err(|e| e.to_string())?
        }
        _ => return Err(format!("{s}: unknown feature; the tool description lists them")),
    })
}

fn view_of(c: &Core, v: &J) -> Result<ViewKey, String> {
    let s = match v {
        J::Null => return Ok(ViewKey::ThreeD),
        v => v.as_str().map(str::trim).unwrap_or_default(),
    };
    let num = |p: &str| s.strip_prefix(p).and_then(|n| n.trim().parse::<i64>().ok());
    let missing = |what: &str, id: i64| CmdError::wire(ErrorCode::NotFound, format!("no {what} {id}"));
    if s == "3d" {
        return Ok(ViewKey::ThreeD);
    }
    if let Some(l) = num("level:") {
        let all = levels(&c.ed.model).map_err(|e| e.to_string())?;
        return all.iter().any(|x| x.0 == l).then_some(ViewKey::Plan(l)).ok_or_else(|| missing("level", l));
    }
    if let Some(v) = num("section:") {
        let q = "SELECT COUNT(*) FROM view_def WHERE id=?1";
        let n: i64 = c.ed.model.sql().query_row(q, [v], |r| r.get(0)).map_err(|e| e.to_string())?;
        return (n > 0).then_some(ViewKey::Section(v)).ok_or_else(|| missing("section", v));
    }
    Err(bad("view", "needs 3d, level:ID or section:ID"))
}

/// A measurement the plan, section and 3D measure tools save (`m.add_measure`).
pub fn measure_save(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let kind = a["kind"].as_str().and_then(MeasureKind::parse).ok_or_else(|| bad("kind", "needs a measurement kind"))?;
    let list = a["anchors"].as_array().ok_or_else(|| bad("anchors", "needs a list of pins"))?;
    if !arity_ok(kind, list.len()) {
        let why = format!("{} measurement: wrong number of pins ({})", kind.code(), list.len());
        return Err(bad("anchors", &why));
    }
    let mut anchors = Vec::with_capacity(list.len());
    for (i, v) in list.iter().enumerate() {
        anchors.push(pin_of(&c.ed.model, v).map_err(|e| bad(&format!("anchors[{i}]"), &e))?);
    }
    let view = view_of(c, &a["view"])?;
    let label = super::fmt::text(a["label"].as_str().unwrap_or_default().trim());
    add_op(c, a, cx, PlanOp::Tool(ToolAct::Measure { kind, view, anchors, label }))
}

// ---- edit_mass ----------------------------------------------------------------------------

/// How a call names a face of a mass.
#[derive(Clone, Debug, PartialEq)]
pub enum FaceSel {
    Tag(u32),
    /// The largest face whose normal points this way.
    Side(P3),
    /// The face whose plane passes through the point (world metres).
    At(P3),
}

fn side(s: &str) -> Option<P3> {
    Some(match s {
        "top" => (0.0, 0.0, 1.0),
        "bottom" => (0.0, 0.0, -1.0),
        "north" => (0.0, 1.0, 0.0),
        "south" => (0.0, -1.0, 0.0),
        "east" => (1.0, 0.0, 0.0),
        "west" => (-1.0, 0.0, 0.0),
        _ => return None,
    })
}

fn dot(a: P3, b: P3) -> f64 {
    a.0 * b.0 + a.1 * b.1 + a.2 * b.2
}

fn find<'f>(faces: &'f [MassFace], f: &FaceSel) -> Option<&'f MassFace> {
    match f {
        FaceSel::Tag(t) => faces.iter().find(|x| x.tag == *t),
        FaceSel::Side(d) => faces
            .iter()
            .filter(|x| dot(x.normal, *d) > 0.9)
            .max_by(|a, b| a.area.total_cmp(&b.area)),
        FaceSel::At(p) => faces
            .iter()
            .filter(|x| x.class != FaceClass::Curved)
            .filter_map(|x| {
                let o = x.outline.first()?.first()?;
                let d = dot(x.normal, (p.0 - o.0, p.1 - o.1, p.2 - o.2)).abs();
                (d <= FACE_TOL).then_some((d, x))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, x)| x),
    }
}

/// The tag of the face a selector names on the mass as it is now.
pub fn face_tag(m: &Model, mass: i64, f: &FaceSel) -> Result<u32, String> {
    let faces = m.mass_faces(mass).map_err(|e| e.to_string())?;
    find(&faces, f).map(|x| x.tag).ok_or_else(|| {
        CmdError::wire(ErrorCode::NotFound, format!("mass:{mass} has no face {f:?}"))
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MassAction {
    PushPull,
    PushRegion,
    Draft,
    Shell,
    Fillet,
    Chamfer,
    Offset,
    SplitFace,
}

impl MassAction {
    fn parse(s: &str) -> Option<MassAction> {
        Some(match s {
            "push_pull" => MassAction::PushPull,
            "push_region" => MassAction::PushRegion,
            "draft" => MassAction::Draft,
            "shell" => MassAction::Shell,
            "fillet" => MassAction::Fillet,
            "chamfer" => MassAction::Chamfer,
            "offset" => MassAction::Offset,
            "split_face" => MassAction::SplitFace,
            _ => return None,
        })
    }
}

/// The call as given; faces resolve on the copy when the item compiles.
#[derive(Clone, Debug, PartialEq)]
pub struct MassEditArgs {
    pub action: MassAction,
    pub faces: Vec<FaceSel>,
    pub distance: Option<f64>,
    /// Degrees.
    pub angle: Option<f64>,
    /// World metres.
    pub region: Vec<P3>,
}

/// A resolved history step; form coordinates are world minus the mass's level elevation.
#[derive(Clone, Debug, PartialEq)]
pub enum FormEdit {
    PushPull { face: u32, d: f64 },
    PushRegion { face: u32, region: Vec<P3>, d: f64 },
    Draft { faces: Vec<u32>, at: P3, pull: P3, angle: f64 },
    Shell { t: f64, open: Vec<u32> },
    Blends { edges: Vec<(u32, u32)>, round: bool, size: f64 },
    Offset { faces: Vec<u32>, d: f64 },
    SplitFace { face: u32, at: P3, n: P3 },
}

impl FormEdit {
    /// The step on top of the mass's history, as the Forms tab writes it.
    pub fn form(&self, base: Box<Form>) -> Form {
        match self.clone() {
            FormEdit::PushPull { face, d } => Form::PushPull { base, face, d, keys: vec![] },
            FormEdit::PushRegion { face, region, d } => {
                Form::PushTo { base, face, region, to: PushGoal::Dist(d), keys: vec![] }
            }
            FormEdit::Draft { faces, at, pull, angle } => Form::Draft { base, faces, at, pull, angle },
            FormEdit::Shell { t, open } => Form::Shell { base, t, open, keys: vec![] },
            FormEdit::Blends { edges, round, size } => Form::Blends { base, edges, round, size, seg: 8 },
            FormEdit::Offset { faces, d } => Form::Offset { base, faces, d, keys: vec![] },
            FormEdit::SplitFace { face, at, n } => Form::SplitFace { base, face, at, n },
        }
    }
}

fn centroid(f: &MassFace) -> P3 {
    let pts: Vec<_> = f.outline.iter().flatten().collect();
    let n = pts.len().max(1) as f64;
    pts.iter().fold((0.0, 0.0, 0.0), |a, p| (a.0 + p.0 / n, a.1 + p.1 / n, a.2 + p.2 / n))
}

fn mass_z0(m: &Model, mass: i64) -> Result<f64, Error> {
    let level = m.mass(mass)?.level;
    Ok(levels(m)?.into_iter().find(|l| l.0 == level).map_or(0.0, |l| l.2))
}

impl MassEditArgs {
    /// Faces, defaults and the neutral plane fixed on `m`, with the defaults of the Forms tab.
    pub fn resolve(&self, m: &Model, mass: i64) -> Result<FormEdit, String> {
        let all = m.mass_faces(mass).map_err(|e| e.to_string())?;
        let mut tags = Vec::with_capacity(self.faces.len());
        for f in &self.faces {
            let x = find(&all, f).ok_or_else(|| {
                CmdError::wire(ErrorCode::NotFound, format!("mass:{mass} has no face {f:?}"))
            })?;
            tags.push(x.tag);
        }
        let face_of = |t: u32| all.iter().find(|x| x.tag == t);
        let one = || match tags.as_slice() {
            [t] => Ok(*t),
            _ => Err(bad("faces", "needs exactly one face")),
        };
        let z0 = mass_z0(m, mass).map_err(|e| e.to_string())?;
        let d = |default: f64| self.distance.unwrap_or(default);
        Ok(match self.action {
            MassAction::PushPull => {
                let d = self.distance.ok_or_else(|| bad("distance", "needs the push in metres"))?;
                if d.abs() < 1e-9 {
                    return Err(CmdError::wire(ErrorCode::NoEffect, "nothing to change: the push is zero"));
                }
                FormEdit::PushPull { face: one()?, d }
            }
            MassAction::PushRegion => {
                if self.region.len() < 3 {
                    return Err(bad("region", "needs three or more [x, y, z] points on the face"));
                }
                let region = self.region.iter().map(|p| (p.0, p.1, p.2 - z0)).collect();
                FormEdit::PushRegion { face: one()?, region, d: d(1.0) }
            }
            MassAction::Draft => {
                // a picked horizontal face is the neutral plane, the base by default
                let zmin = all.iter().flat_map(|f| f.outline.iter().flatten()).fold(f64::INFINITY, |a, p| a.min(p.2));
                let (mut at, mut pull) = ((0.0, 0.0, zmin - z0), (0.0, 0.0, 1.0));
                let mut tilt = vec![];
                for f in tags.iter().filter_map(|t| face_of(*t)) {
                    if matches!(f.class, FaceClass::Up | FaceClass::Down) {
                        let c = centroid(f);
                        at = (c.0, c.1, c.2 - z0);
                        pull = f.normal;
                    } else {
                        tilt.push(f.tag);
                    }
                }
                if tilt.is_empty() || !zmin.is_finite() {
                    return Err(bad("faces", "needs one or more side faces to tilt"));
                }
                let angle = self.angle.unwrap_or(5.0).to_radians();
                FormEdit::Draft { faces: tilt, at, pull, angle }
            }
            MassAction::Shell => FormEdit::Shell { t: d(0.3), open: tags },
            MassAction::Fillet | MassAction::Chamfer => {
                if tags.is_empty() || tags.len() % 2 != 0 {
                    return Err(bad("faces", "needs face pairs: each pair names the edge between them"));
                }
                let round = self.action == MassAction::Fillet;
                let edges = tags.chunks_exact(2).map(|c| (c[0], c[1])).collect();
                FormEdit::Blends { edges, round, size: d(if round { 0.5 } else { 0.3 }) }
            }
            MassAction::Offset => {
                if tags.is_empty() {
                    return Err(bad("faces", "needs one or more faces"));
                }
                FormEdit::Offset { faces: tags, d: d(0.5) }
            }
            MassAction::SplitFace => {
                let f = face_of(one()?).ok_or_else(|| bad("faces", "needs a face of the mass"))?;
                let c = centroid(f);
                // vertical faces split by a horizontal plane, others along x
                let n = if f.class == FaceClass::Vertical { (0.0, 0.0, 1.0) } else { (1.0, 0.0, 0.0) };
                FormEdit::SplitFace { face: f.tag, at: (c.0, c.1, c.2 - z0), n }
            }
        })
    }
}

fn face_sel(v: &J) -> Option<FaceSel> {
    if let Some(t) = v.as_u64() {
        return u32::try_from(t).ok().map(FaceSel::Tag);
    }
    if let Some(s) = v.as_str() {
        return side(s).map(FaceSel::Side);
    }
    p3(v).map(FaceSel::At)
}

/// One history step of the Forms tab on a mass (`m.edit_mass_form`).
pub fn edit_mass(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let mass = mass_arg(c, a)?;
    let action = a["action"].as_str().and_then(MassAction::parse).ok_or_else(|| bad("action", "needs a mass edit"))?;
    let mut faces = vec![];
    for (i, v) in a["faces"].as_array().map(Vec::as_slice).unwrap_or_default().iter().enumerate() {
        faces.push(face_sel(v).ok_or_else(|| bad(&format!("faces[{i}]"), "needs a tag, a side or [x, y, z]"))?);
    }
    let mut region = vec![];
    for (i, v) in a["region"].as_array().map(Vec::as_slice).unwrap_or_default().iter().enumerate() {
        region.push(p3(v).ok_or_else(|| bad(&format!("region[{i}]"), "needs [x, y, z] in metres"))?);
    }
    let args = MassEditArgs {
        action,
        faces,
        distance: opt_num(a, "distance")?,
        angle: opt_num(a, "angle")?,
        region,
    };
    // refused now when it cannot resolve on the model as it is; the item resolves again on the copy
    args.resolve(&c.ed.model, mass)?;
    add_op(c, a, cx, PlanOp::Tool(ToolAct::MassEdit { mass, args }))
}

// ---- mass_to_family -----------------------------------------------------------------------

/// The history panel's timeline to family (`elem::history::to_family`).
pub fn mass_to_family(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let mass = mass_arg(c, a)?;
    let has = c.ed.model.mass_timeline(mass).map_err(|e| e.to_string())?.is_some();
    if !has {
        return Err(bad("mass", &format!("mass:{mass} has no feature timeline")));
    }
    let name = match &a["name"] {
        J::Null => None,
        v => match v.as_str().map(str::trim) {
            Some(n) if !n.is_empty() => Some(n.to_string()),
            _ => return Err(bad("name", "needs a family name")),
        },
    };
    add_op(c, a, cx, PlanOp::Tool(ToolAct::TimelineFamily { mass, name }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::MassSpec;

    /// A 10 x 6 x 9 m box on a level at 3 m.
    fn boxed() -> (Model, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let level = m.add_level("L1", 3.0).unwrap();
        let ring = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)];
        let form = Form::Extrude { ring, z0: 0.0, h: 9.0, ids: vec![] };
        let spec = MassSpec { level, name: "Box".into(), void: false, inplace: false, form };
        let id = m.add_mass(&spec).unwrap();
        (m, id)
    }

    fn args(action: MassAction, faces: Vec<FaceSel>) -> MassEditArgs {
        MassEditArgs { action, faces, distance: None, angle: None, region: vec![] }
    }

    #[test]
    fn a_side_a_point_and_a_tag_name_the_same_face() {
        let (m, id) = boxed();
        let top = face_tag(&m, id, &FaceSel::Side((0.0, 0.0, 1.0))).unwrap();
        assert_eq!(face_tag(&m, id, &FaceSel::At((4.0, 2.0, 12.0))).unwrap(), top);
        assert_eq!(face_tag(&m, id, &FaceSel::Tag(top)).unwrap(), top);
        assert!(face_tag(&m, id, &FaceSel::At((4.0, 2.0, 30.0))).is_err());
    }

    #[test]
    fn edits_take_the_forms_tab_defaults_in_level_coordinates() {
        let (mut m, id) = boxed();
        let mut a = args(MassAction::PushPull, vec![FaceSel::Side((0.0, 0.0, 1.0))]);
        assert!(a.resolve(&m, id).is_err(), "a push needs its distance");
        a.distance = Some(2.0);
        let e = a.resolve(&m, id).unwrap();
        m.edit_mass_form(id, |b| e.form(b)).unwrap();
        let top = m.mass_faces(id).unwrap().into_iter().find(|f| f.normal.2 > 0.9).unwrap();
        assert!((top.outline[0][0].2 - 14.0).abs() < 1e-6, "{top:?}");
        let sides = vec![FaceSel::Side((0.0, -1.0, 0.0)), FaceSel::Side((1.0, 0.0, 0.0))];
        match args(MassAction::Chamfer, sides).resolve(&m, id).unwrap() {
            FormEdit::Blends { edges, round, size } => {
                assert_eq!((edges.len(), round, size), (1, false, 0.3));
            }
            e => panic!("{e:?}"),
        }
        match args(MassAction::Draft, vec![FaceSel::Side((0.0, 1.0, 0.0))]).resolve(&m, id).unwrap() {
            FormEdit::Draft { faces, at, pull, angle } => {
                assert_eq!((faces.len(), at.2, pull), (1, 0.0, (0.0, 0.0, 1.0)));
                assert!((angle - 5f64.to_radians()).abs() < 1e-12);
            }
            e => panic!("{e:?}"),
        }
        assert!(args(MassAction::Fillet, vec![FaceSel::Tag(0)]).resolve(&m, id).is_err());
    }

    #[test]
    fn pins_name_features_that_follow_their_element() {
        let (m, id) = boxed();
        assert!(matches!(pin_of(&m, &serde_json::json!(format!("mass:{id}@top"))), Ok(A::MassFace { .. })));
        assert_eq!(
            pin_of(&m, &serde_json::json!("wall:4@face1")),
            Ok(A::Dim(DimRef::WallEnd { wall: 4, face: 1, end: 0 }))
        );
        assert_eq!(pin_of(&m, &serde_json::json!([1.0, 2.0, 3.0])), Ok(A::Point([1.0, 2.0, 3.0])));
        assert!(pin_of(&m, &serde_json::json!("wall:4@middle")).is_err());
        assert!(arity_ok(MeasureKind::Angle, 3) && !arity_ok(MeasureKind::Volume, 2));
    }
}
