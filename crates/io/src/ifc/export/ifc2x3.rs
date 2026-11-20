// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! IFC4 entities -> IFC2X3: attribute lists cut to the older schema, an owner history on every
//! rooted entity, generic flow classes for MEP, tessellated bodies as faceted breps, indexed
//! curves as polylines (arcs sampled), boundary conditions as stiffness numbers. What an
//! IFC2X3 attribute list cannot hold (the axis of a structural curve member) goes to a
//! property set and is named in the notes returned with the file.

use std::collections::{HashMap, HashSet};

use super::post::{add_pset, Emit};
use crate::ifc::reader::{refs_of, StepFile, Value};
use crate::ifc::step::{is_global_id, real, text};

const NONE: &str = "$";

pub(super) fn ser(v: &Value) -> String {
    let join = |l: &[Value]| l.iter().map(ser).collect::<Vec<_>>().join(",");
    match v {
        Value::Null => NONE.into(),
        Value::Derived => "*".into(),
        Value::Int(i) => i.to_string(),
        Value::Real(x) => real(*x),
        Value::Str(s) => text(s),
        Value::Enum(e) => format!(".{e}."),
        Value::Ref(r) => format!("#{r}"),
        Value::List(l) => format!("({})", join(l)),
        Value::Typed(n, a) => format!("{n}({})", join(a)),
        Value::Binary(b) => format!("\"{b}\""),
    }
}

/// Attribute count and optional new class name in IFC2X3; `pad` adds one `$` attribute.
fn rule(class: &str) -> Option<(Option<&'static str>, usize, bool)> {
    Some(match class {
        "IFCWALL"
        | "IFCCOLUMN"
        | "IFCBEAM"
        | "IFCMEMBER"
        | "IFCPLATE"
        | "IFCOPENINGELEMENT"
        | "IFCCURTAINWALL"
        | "IFCRAMPFLIGHT"
        | "IFCDISTRIBUTIONPORT" => (None, 8, false),
        "IFCVIRTUALELEMENT" | "IFCDISCRETEACCESSORY" => (None, 8, false),
        "IFCDOOR" | "IFCWINDOW" => (None, 10, false),
        "IFCSTAIRFLIGHT" => (None, 12, false),
        "IFCGEOGRAPHICELEMENT" | "IFCBUILDINGELEMENTPROXY" => {
            (Some("IFCBUILDINGELEMENTPROXY"), 8, true)
        }
        "IFCPIPESEGMENT" | "IFCDUCTSEGMENT" | "IFCCABLECARRIERSEGMENT" => {
            (Some("IFCFLOWSEGMENT"), 8, false)
        }
        "IFCPIPEFITTING" | "IFCDUCTFITTING" | "IFCCABLECARRIERFITTING" => {
            (Some("IFCFLOWFITTING"), 8, false)
        }
        "IFCDISTRIBUTIONSYSTEM" => (Some("IFCSYSTEM"), 5, false),
        "IFCSTRUCTURALPOINTCONNECTION" => (None, 8, false),
        "IFCSTRUCTURALANALYSISMODEL" => (None, 9, false),
        "IFCZONE" => (None, 5, false),
        "IFCGRID" => (None, 10, false),
        "IFCQUANTITYLENGTH" | "IFCQUANTITYAREA" | "IFCQUANTITYVOLUME" | "IFCQUANTITYCOUNT"
        | "IFCQUANTITYWEIGHT" | "IFCQUANTITYTIME" => (None, 4, false),
        "IFCCLASSIFICATION" | "IFCCLASSIFICATIONREFERENCE" => (None, 4, false),
        "IFCMATERIAL" => (None, 1, false),
        "IFCMATERIALLAYER" => (None, 3, false),
        "IFCMATERIALLAYERSET" => (None, 2, false),
        "IFCMATERIALLAYERSETUSAGE" => (None, 4, false),
        _ => return None,
    })
}

/// Component type classes written by `comp_types`.
fn component_type(class: &str) -> bool {
    matches!(
        class,
        "IFCFURNITURETYPE"
            | "IFCSYSTEMFURNITUREELEMENTTYPE"
            | "IFCBUILDINGELEMENTPROXYTYPE"
            | "IFCSANITARYTERMINALTYPE"
            | "IFCLIGHTFIXTURETYPE"
            | "IFCAIRTERMINALTYPE"
            | "IFCAIRTERMINALBOXTYPE"
            | "IFCCOVERINGTYPE"
            | "IFCMEMBERTYPE"
            | "IFCRAILINGTYPE"
            | "IFCSHADINGDEVICETYPE"
            | "IFCSLABTYPE"
            | "IFCGEOGRAPHICELEMENTTYPE"
            | "IFCOUTLETTYPE"
            | "IFCELECTRICDISTRIBUTIONBOARDTYPE"
            | "IFCSPACEHEATERTYPE"
            | "IFCSWITCHINGDEVICETYPE"
            | "IFCUNITARYEQUIPMENTTYPE"
            | "IFCFIRESUPPRESSIONTERMINALTYPE"
            | "IFCLAMPTYPE"
            | "IFCELECTRICAPPLIANCETYPE"
            | "IFCMEDICALDEVICETYPE"
            | "IFCTRANSPORTELEMENTTYPE"
            | "IFCTANKTYPE"
            // the other component types (air terminal box, covering, member, railing, slab,
            // fittings) keep the IFC4 layout in IFC2X3
            | "IFCGEOGRAPHICELEMENTTYPE"
            | "IFCSHADINGDEVICETYPE"
            | "IFCSTAIRTYPE"
    )
}

/// IFC2X3 form of a component type: furniture keeps AssemblyPlace and has no PredefinedType,
/// a system furniture type has neither, the IFC4-only types (distribution board, geographic
/// element, shading device, stair) become proxy types, and the older enumerations take NOTDEFINED.
fn component_type_2x3<'a>(class: &'a str, a: &mut Vec<Value>) -> &'a str {
    a.truncate(9);
    match class {
        "IFCFURNITURETYPE" => {
            a.push(Value::Enum("NOTDEFINED".into()));
            class
        }
        "IFCSYSTEMFURNITUREELEMENTTYPE" => class,
        // IFC4-only type objects
        "IFCELECTRICDISTRIBUTIONBOARDTYPE"
        | "IFCGEOGRAPHICELEMENTTYPE"
        | "IFCSHADINGDEVICETYPE"
        | "IFCMEDICALDEVICETYPE"
        | "IFCSTAIRTYPE" => {
            a.push(Value::Enum("NOTDEFINED".into()));
            "IFCBUILDINGELEMENTPROXYTYPE"
        }
        _ => {
            a.push(Value::Enum("NOTDEFINED".into()));
            class
        }
    }
}

fn dropped(class: &str) -> bool {
    matches!(
        class,
        "IFCMAPCONVERSION"
            | "IFCPROJECTEDCRS"
            | "IFCMATERIALPROPERTIES"
            | "IFCCARTESIANPOINTLIST2D"
            | "IFCCARTESIANPOINTLIST3D"
            | "IFCINDEXEDPOLYGONALFACE"
            | "IFCINDEXEDPOLYGONALFACEWITHVOIDS"
    )
}

fn rows(v: Option<&Value>) -> Vec<Vec<f64>> {
    v.and_then(Value::as_list)
        .map(|l| {
            l.iter()
                .map(|r| {
                    r.as_list()
                        .map(|c| c.iter().filter_map(Value::as_f64).collect())
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn point(e: &mut Emit, c: &[f64]) -> u32 {
    let v: Vec<String> = c.iter().map(|x| real(*x)).collect();
    e.add("IFCCARTESIANPOINT", &[format!("({})", v.join(","))])
}

/// Points of an arc through a, b, c (a excluded, c included), chord length about 0.2 rad.
fn arc_points(a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> Vec<(f64, f64)> {
    let d = 2.0 * (a.0 * (b.1 - c.1) + b.0 * (c.1 - a.1) + c.0 * (a.1 - b.1));
    if d.abs() < 1e-12 {
        return vec![b, c];
    }
    let sq = |p: (f64, f64)| p.0 * p.0 + p.1 * p.1;
    let ux = (sq(a) * (b.1 - c.1) + sq(b) * (c.1 - a.1) + sq(c) * (a.1 - b.1)) / d;
    let uy = (sq(a) * (c.0 - b.0) + sq(b) * (a.0 - c.0) + sq(c) * (b.0 - a.0)) / d;
    let rad = (a.0 - ux).hypot(a.1 - uy);
    let ang = |p: (f64, f64)| (p.1 - uy).atan2(p.0 - ux);
    let (t0, tm, t1) = (ang(a), ang(b), ang(c));
    let tau = std::f64::consts::TAU;
    let ccw = (tm - t0).rem_euclid(tau) < (t1 - t0).rem_euclid(tau);
    let sweep = if ccw {
        (t1 - t0).rem_euclid(tau)
    } else {
        -(t0 - t1).rem_euclid(tau)
    };
    let n = ((sweep.abs() / 0.2).ceil() as usize).clamp(2, 64);
    let mut out: Vec<(f64, f64)> = (1..=n)
        .map(|i| {
            let t = t0 + sweep * i as f64 / n as f64;
            (ux + rad * t.cos(), uy + rad * t.sin())
        })
        .collect();
    if let Some(l) = out.last_mut() {
        *l = c;
    }
    out
}

/// Polyline through the indexed curve; the new points go to `e`.
fn polyline(f: &StepFile, e: &mut Emit, a: &[Value]) -> String {
    let pts = a
        .first()
        .and_then(Value::as_ref)
        .map(|l| rows(f.get(l).and_then(|x| x.args().first())))
        .unwrap_or_default();
    let xy = |i: usize| (pts[i - 1][0], pts[i - 1][1]);
    let mut seq: Vec<(f64, f64)> = vec![];
    let push = |p: (f64, f64), seq: &mut Vec<(f64, f64)>| {
        if seq.last() != Some(&p) {
            seq.push(p);
        }
    };
    match a.get(1).and_then(Value::as_list) {
        Some(segs) if !segs.is_empty() => {
            for sg in segs {
                let Value::Typed(kind, v) = sg else { continue };
                let idx: Vec<usize> = v
                    .first()
                    .and_then(Value::as_list)
                    .map(|l| {
                        l.iter()
                            .filter_map(|x| match x {
                                Value::Int(i) if *i >= 1 && (*i as usize) <= pts.len() => {
                                    Some(*i as usize)
                                }
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if kind == "IFCARCINDEX" && idx.len() == 3 {
                    push(xy(idx[0]), &mut seq);
                    for p in arc_points(xy(idx[0]), xy(idx[1]), xy(idx[2])) {
                        push(p, &mut seq);
                    }
                } else {
                    for i in idx {
                        push(xy(i), &mut seq);
                    }
                }
            }
        }
        _ => {
            for i in 1..=pts.len() {
                push(xy(i), &mut seq);
            }
        }
    }
    let ids: Vec<String> = seq
        .iter()
        .map(|p| format!("#{}", point(e, &[p.0, p.1])))
        .collect();
    format!("({})", ids.join(","))
}

/// Faceted brep (closed) or shell model (open) lines for a tessellated face set.
fn face_set(f: &StepFile, e: &mut Emit, a: &[Value]) -> (&'static str, String) {
    let coords = a
        .first()
        .and_then(Value::as_ref)
        .map(|l| rows(f.get(l).and_then(|x| x.args().first())))
        .unwrap_or_default();
    let pts: Vec<u32> = coords.iter().map(|c| point(e, c)).collect();
    let closed = matches!(a.get(2), Some(Value::Enum(t)) if t == "T");
    let mut faces = vec![];
    for tri in a.get(3).and_then(Value::as_list).unwrap_or_default() {
        let idx: Vec<u32> = tri
            .as_list()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| match v {
                Value::Int(i) if *i >= 1 && (*i as usize) <= pts.len() => {
                    Some(pts[*i as usize - 1])
                }
                _ => None,
            })
            .collect();
        if idx.len() < 3 {
            continue;
        }
        let ring: Vec<String> = idx.iter().map(|p| format!("#{p}")).collect();
        let lp = e.add("IFCPOLYLOOP", &[format!("({})", ring.join(","))]);
        let bd = e.add("IFCFACEOUTERBOUND", &[format!("#{lp}"), ".T.".into()]);
        faces.push(format!("#{}", e.add("IFCFACE", &[format!("(#{bd})")])));
    }
    let list = format!("({})", faces.join(","));
    if closed {
        let sh = e.add("IFCCLOSEDSHELL", &[list]);
        ("IFCFACETEDBREP", format!("#{sh}"))
    } else {
        let sh = e.add("IFCOPENSHELL", &[list]);
        ("IFCSHELLBASEDSURFACEMODEL", format!("(#{sh})"))
    }
}

/// Faceted brep (closed) or shell model (open) for a polygonal face set: one face per
/// indexed face, its voids as inner bounds.
fn polygonal_set(f: &StepFile, e: &mut Emit, a: &[Value]) -> (&'static str, String) {
    let coords = a
        .first()
        .and_then(Value::as_ref)
        .map(|l| rows(f.get(l).and_then(|x| x.args().first())))
        .unwrap_or_default();
    let pts: Vec<u32> = coords.iter().map(|c| point(e, c)).collect();
    let closed = matches!(a.get(1), Some(Value::Enum(t)) if t == "T");
    let ring = |v: &Value| -> Vec<String> {
        v.as_list()
            .unwrap_or_default()
            .iter()
            .filter_map(|x| match x {
                Value::Int(i) if *i >= 1 && (*i as usize) <= pts.len() => {
                    Some(format!("#{}", pts[*i as usize - 1]))
                }
                _ => None,
            })
            .collect()
    };
    let mut faces = vec![];
    for fid in a.get(2).map(Value::refs).unwrap_or_default() {
        let fa = f.args(fid);
        let Some(outer) = fa.first().map(ring).filter(|r| r.len() >= 3) else {
            continue;
        };
        let lp = e.add("IFCPOLYLOOP", &[format!("({})", outer.join(","))]);
        let ob = e.add("IFCFACEOUTERBOUND", &[format!("#{lp}"), ".T.".into()]);
        let mut bounds = vec![format!("#{ob}")];
        for h in fa.get(1).and_then(Value::as_list).unwrap_or_default() {
            let hr = ring(h);
            if hr.len() >= 3 {
                let hl = e.add("IFCPOLYLOOP", &[format!("({})", hr.join(","))]);
                let ib = e.add("IFCFACEBOUND", &[format!("#{hl}"), ".T.".into()]);
                bounds.push(format!("#{ib}"));
            }
        }
        let face = e.add("IFCFACE", &[format!("({})", bounds.join(","))]);
        faces.push(format!("#{face}"));
    }
    let list = format!("({})", faces.join(","));
    if closed {
        let sh = e.add("IFCCLOSEDSHELL", &[list]);
        ("IFCFACETEDBREP", format!("#{sh}"))
    } else {
        let sh = e.add("IFCOPENSHELL", &[list]);
        ("IFCSHELLBASEDSURFACEMODEL", format!("(#{sh})"))
    }
}

/// `#id=CLASS(args);`
pub(super) fn line(id: u32, class: &str, args: &[Value]) -> String {
    let args: Vec<String> = args.iter().map(ser).collect();
    format!("#{id}={class}({});", args.join(","))
}

/// Adds the owner history every rooted entity needs and returns its id.
pub(super) fn owner_history(e: &mut Emit) -> u32 {
    let person = e.add(
        "IFCPERSON",
        &[
            text("ddc"),
            text("ddc"),
            NONE.into(),
            NONE.into(),
            NONE.into(),
            NONE.into(),
            NONE.into(),
            NONE.into(),
        ],
    );
    let org = e.add(
        "IFCORGANIZATION",
        &[
            NONE.into(),
            text("ddc"),
            NONE.into(),
            NONE.into(),
            NONE.into(),
        ],
    );
    let po = e.add(
        "IFCPERSONANDORGANIZATION",
        &[format!("#{person}"), format!("#{org}"), NONE.into()],
    );
    let app = e.add(
        "IFCAPPLICATION",
        &[format!("#{org}"), text("1"), text("ddc-io"), text("ddc-io")],
    );
    e.add(
        "IFCOWNERHISTORY",
        &[
            format!("#{po}"),
            format!("#{app}"),
            NONE.into(),
            ".ADDED.".into(),
            NONE.into(),
            NONE.into(),
            NONE.into(),
            "0".into(),
        ],
    )
}

/// IFC2X3 holds doors and windows as styles of 12 attributes.
fn opening_style(class: &str, a: &mut Vec<Value>) -> &'static str {
    a.truncate(8);
    for x in ["NOTDEFINED", "NOTDEFINED", "F", "F"] {
        a.push(Value::Enum(x.into()));
    }
    if class == "IFCDOORTYPE" {
        "IFCDOORSTYLE"
    } else {
        "IFCWINDOWSTYLE"
    }
}

/// IFC2X3 holds presentation style assignments, not the styles themselves.
fn wrap_styles(e: &mut Emit, a: &mut [Value]) {
    if let Some(Value::List(styles)) = a.get_mut(1) {
        for st in styles.iter_mut() {
            if let Value::Ref(style) = st {
                let wrap = e.add("IFCPRESENTATIONSTYLEASSIGNMENT", &[format!("(#{style})")]);
                *st = Value::Ref(wrap);
            }
        }
    }
}

/// Brep when every item is a closed tessellated set, SurfaceModel when one is open.
fn representation_type(f: &StepFile, closed: &HashMap<u32, bool>, a: &mut [Value]) {
    let items = a.get(3).map(Value::refs).unwrap_or_default();
    if items.iter().any(|i| f.type_of(*i) == Some("IFCSWEPTDISKSOLID")) {
        a[2] = Value::Str("SweptSolid".into());
        return;
    }
    if items.iter().any(|i| closed.contains_key(i)) {
        let all = items
            .iter()
            .all(|i| closed.get(i).copied().unwrap_or(false));
        a[2] = Value::Str(if all { "Brep" } else { "SurfaceModel" }.into());
    }
}

/// Stiffness value of a fixed degree of freedom. IFC2X3 TC1, IfcBoundaryNodeCondition: "value
/// (-1.) represents an infinitely large value - or a fixed connectivity with infinitive
/// stiffness; value zero (0.) represents no stiffness or a free connectivity".
const RIGID: f64 = -1.0;

/// IFC4 states each degree of freedom of a boundary condition as fixed (`.T.`) or free; IFC2X3
/// states a stiffness.
fn stiffness(a: &mut [Value]) {
    for v in a.iter_mut().skip(1) {
        let fixed = match v {
            Value::Typed(t, x) if t == "IFCBOOLEAN" => {
                matches!(x.first(), Some(Value::Enum(c)) if c == "T")
            }
            _ => continue,
        };
        *v = Value::Real(if fixed { RIGID } else { 0.0 });
    }
}

const STIFFNESS_NOTE: &str =
    "IFC2X3 boundary conditions are stiffness numbers: fixed is written as -1 (infinite \
     stiffness) and free as 0, as IfcBoundaryNodeCondition defines";
const AXIS_NOTE: &str =
    "IFC2X3 structural curve members have no axis: it is written to DDC_StructuralMember";

fn note(notes: &mut Vec<String>, text: &str) {
    if !notes.iter().any(|n| n == text) {
        notes.push(text.to_string());
    }
}

/// Local axis of structural curve member `id`, which IFC2X3 has no attribute for, as a
/// property set of the member; the axis is the last attribute of `a` and is removed.
fn member_axis(f: &StepFile, e: &mut Emit, oh: u32, id: u32, a: &mut Vec<Value>) {
    let axis = a.pop().and_then(|v| v.as_ref());
    let ratios: Vec<f64> = axis
        .and_then(|d| f.args(d).first())
        .and_then(Value::as_list)
        .map(|l| l.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    let Some(Value::Str(guid)) = a.first() else {
        return;
    };
    let props: Vec<(&str, String)> = ["LocalAxisX", "LocalAxisY", "LocalAxisZ"]
        .into_iter()
        .zip(ratios)
        .map(|(n, x)| (n, format!("IFCREAL({})", real(x))))
        .collect();
    let key = format!("pset:DDC_StructuralMember:{guid}");
    add_pset(e, &format!("#{oh}"), &key, "DDC_StructuralMember", &props, id);
}

const PLACEMENT_NOTE: &str =
    "IFC2X3 analysis models have no shared placement: it is written to DDC_AnalysisModel";

/// Numbers of the first list attribute of entity `r` (a point or a direction).
fn coords(f: &StepFile, r: Option<&Value>) -> Vec<f64> {
    r.and_then(Value::as_ref)
        .and_then(|d| f.args(d).first())
        .and_then(Value::as_list)
        .map(|l| l.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default()
}

/// Shared placement of analysis model `id`, which IFC2X3 has no attribute for, as a property
/// set of the model: location, axis and reference direction relative to its parent placement.
/// The placement is the last attribute of `a` and is removed.
fn model_placement(f: &StepFile, e: &mut Emit, oh: u32, id: u32, a: &mut Vec<Value>) {
    let place = a.pop().and_then(|v| v.as_ref());
    let frame = place.and_then(|p| f.args(p).get(1)).and_then(Value::as_ref);
    let Some(Value::Str(guid)) = a.first() else {
        return;
    };
    let Some(frame) = frame else {
        return;
    };
    let at = f.args(frame);
    let mut props: Vec<(&str, String)> = vec![];
    for (names, v) in [
        (["LocationX", "LocationY", "LocationZ"], coords(f, at.first())),
        (["AxisX", "AxisY", "AxisZ"], coords(f, at.get(1))),
        (["RefDirectionX", "RefDirectionY", "RefDirectionZ"], coords(f, at.get(2))),
    ] {
        for (n, x) in names.into_iter().zip(v) {
            props.push((n, format!("IFCREAL({})", real(x))));
        }
    }
    let key = format!("pset:DDC_AnalysisModel:{guid}");
    add_pset(e, &format!("#{oh}"), &key, "DDC_AnalysisModel", &props, id);
}

/// What the conversion learns from the entities before it converts the rest.
#[derive(Default)]
struct Facts {
    /// Closed flag of each tessellated set: it decides the shape representation type.
    closed: HashMap<u32, bool>,
    /// Last parameter of each curve written as a polyline (one per point), for the swept disk
    /// whose IFC2X3 start and end parameters are required.
    ends: HashMap<u32, f64>,
    /// Exact NURBS bodies and what only they use: IFC2X3 has no form for them.
    gone: HashSet<u32>,
    /// Triangulated 'Body-FallBack' representations that become the 'Body'.
    fallback: HashSet<u32>,
}

fn deep_refs(v: &Value, out: &mut Vec<u32>) {
    match v {
        Value::Ref(r) => out.push(*r),
        Value::List(l) | Value::Typed(_, l) => l.iter().for_each(|x| deep_refs(x, out)),
        _ => {}
    }
}

fn all_refs(f: &StepFile, id: u32) -> Vec<u32> {
    let mut out = vec![];
    f.args(id).iter().for_each(|v| deep_refs(v, &mut out));
    out
}

fn str_at<'a>(f: &'a StepFile, id: u32, i: usize) -> Option<&'a str> {
    match f.args(id).get(i) {
        Some(Value::Str(s)) => Some(s.as_str()),
        _ => None,
    }
}

/// Advanced (B-spline) body representations, every entity only they reach, and the
/// fallback representations beside them.
fn advanced(f: &StepFile, facts: &mut Facts) {
    let reps: HashSet<u32> = f
        .iter()
        .filter(|(id, t)| {
            *t == "IFCSHAPEREPRESENTATION"
                && matches!(str_at(f, *id, 2), Some("AdvancedBrep" | "AdvancedSurface"))
        })
        .map(|(id, _)| id)
        .collect();
    if reps.is_empty() {
        return;
    }
    let mut d: HashSet<u32> = reps.clone();
    let mut stack: Vec<u32> = reps.iter().copied().collect();
    while let Some(id) = stack.pop() {
        for r in all_refs(f, id) {
            if d.insert(r) {
                stack.push(r);
            }
        }
    }
    // what the rest of the file still reaches inside `d` stays
    let mut keep: HashSet<u32> = HashSet::new();
    let mut stack = vec![];
    for (id, t) in f.iter() {
        if d.contains(&id) {
            continue;
        }
        for r in all_refs(f, id) {
            let cut = t == "IFCPRODUCTDEFINITIONSHAPE" && reps.contains(&r);
            if !cut && d.contains(&r) && keep.insert(r) {
                stack.push(r);
            }
        }
        if t == "IFCPRODUCTDEFINITIONSHAPE" {
            let list = refs_of(f.args(id).get(2));
            if list.iter().any(|r| reps.contains(r)) {
                facts.fallback.extend(
                    list.into_iter().filter(|r| str_at(f, *r, 1) == Some("Body-FallBack")),
                );
            }
        }
    }
    while let Some(id) = stack.pop() {
        for r in all_refs(f, id) {
            if d.contains(&r) && keep.insert(r) {
                stack.push(r);
            }
        }
    }
    facts.gone = d.difference(&keep).copied().collect();
}

/// IFC2X3 requires the parameter range of a swept disk; IFC4 leaves it out for the whole curve.
fn sweep_range(facts: &Facts, a: &mut [Value]) {
    let end = a.first().and_then(Value::as_ref).and_then(|c| facts.ends.get(&c));
    if let (Some(end), true) = (end, a.len() == 5) {
        a[3] = Value::Real(0.0);
        a[4] = Value::Real(*end);
    }
}

/// A B-spline curve with knots (IFC4 only) as the points of a polyline along it, chord sag
/// within the arcs' one; `None` when the attributes do not make a planar curve.
fn spline_points(f: &StepFile, a: &[Value]) -> Option<Vec<(f64, f64)>> {
    let degree = a.first()?.as_f64()? as usize;
    let ctrl: Vec<ddc_geom::V3> = a
        .get(1)?
        .as_list()?
        .iter()
        .map(|p| {
            let c = f.args(p.as_ref()?).first()?.as_list()?;
            Some(ddc_geom::v3(c.first()?.as_f64()?, c.get(1)?.as_f64()?, 0.0))
        })
        .collect::<Option<_>>()?;
    let mult = a.get(5)?.as_list()?;
    let vals = a.get(6)?.as_list()?;
    let mut knots = vec![];
    for (m, v) in mult.iter().zip(vals) {
        let (m, v) = (m.as_f64()? as usize, v.as_f64()?);
        knots.extend(std::iter::repeat_n(v, m));
    }
    let weights: Vec<f64> = match a.get(8).and_then(Value::as_list) {
        Some(w) => w.iter().map(Value::as_f64).collect::<Option<_>>()?,
        None => vec![1.0; ctrl.len()],
    };
    let cv = ddc_geom::nurbs::Curve::new(degree, knots, ctrl, weights).ok()?;
    let ax = ddc_geom::wall_spline::SplineAxis::new(cv).ok()?;
    let pts = ax.offset_polyline(0.0, ddc_geom::arc::CHORD_SAG);
    Some(pts.iter().map(|p| (p.x, p.y)).collect())
}

/// One entity as IFC2X3 text.
fn convert(
    f: &StepFile,
    e: &mut Emit,
    facts: &Facts,
    oh: u32,
    id: u32,
    notes: &mut Vec<String>,
) -> String {
    let class = f.type_of(id).unwrap_or_default();
    let mut a = f.args(id).to_vec();
    let mut name = class;
    match class {
        "IFCINDEXEDPOLYCURVE" => {
            let pl = polyline(f, e, &a);
            return format!("#{id}=IFCPOLYLINE({pl});");
        }
        "IFCBSPLINECURVEWITHKNOTS" | "IFCRATIONALBSPLINECURVEWITHKNOTS" => {
            if let Some(pts) = spline_points(f, &a) {
                let ids: Vec<String> =
                    pts.iter().map(|p| format!("#{}", point(e, &[p.0, p.1]))).collect();
                return format!("#{id}=IFCPOLYLINE(({}));", ids.join(","));
            }
        }
        "IFCTRIANGULATEDFACESET" => {
            let (n, arg) = face_set(f, e, &a);
            return format!("#{id}={n}({arg});");
        }
        "IFCPOLYGONALFACESET" => {
            let (n, arg) = polygonal_set(f, e, &a);
            return format!("#{id}={n}({arg});");
        }
        "IFCDOORTYPE" | "IFCWINDOWTYPE" => name = opening_style(class, &mut a),
        c if component_type(c) => name = component_type_2x3(class, &mut a),
        "IFCSTYLEDITEM" => wrap_styles(e, &mut a),
        "IFCSPACE" => {
            if let Some(v) = a.get_mut(9).filter(|v| !matches!(v, Value::Null)) {
                *v = Value::Enum("INTERNAL".into());
            }
        }
        "IFCSHAPEREPRESENTATION" => {
            if facts.fallback.contains(&id) {
                a[1] = Value::Str("Body".into());
            }
            representation_type(f, &facts.closed, &mut a)
        }
        "IFCPRODUCTDEFINITIONSHAPE" if !facts.gone.is_empty() => {
            if let Some(Value::List(l)) = a.get_mut(2) {
                l.retain(|v| !matches!(v, Value::Ref(r) if facts.gone.contains(r)));
            }
        }
        // no shading device in IFC2X3: a proxy that names it in ObjectType
        "IFCSHADINGDEVICE" => {
            let pre = match a.get(8) {
                Some(Value::Enum(p)) if p != "NOTDEFINED" && p != "USERDEFINED" => format!(".{p}"),
                _ => String::new(),
            };
            a.truncate(8);
            a[4] = Value::Str(format!("IfcShadingDevice{pre}"));
            a.push(Value::Null);
            name = "IFCBUILDINGELEMENTPROXY";
        }
        // IFC2X3: OperationType of the older enumeration, then the two capacities
        "IFCTRANSPORTELEMENT" => {
            let op = match a.get(8) {
                Some(Value::Enum(p)) if matches!(p.as_str(), "ELEVATOR" | "ESCALATOR" | "MOVINGWALKWAY" | "USERDEFINED") => p.clone(),
                _ => "NOTDEFINED".into(),
            };
            a.truncate(8);
            a.extend([Value::Enum(op), Value::Null, Value::Null]);
        }
        "IFCMECHANICALFASTENER" => a.truncate(10),
        // IFC4 types IFC2X3 lacks
        "IFCCOVERING" | "IFCFOOTING" => {
            if let Some(Value::Enum(p)) = a.get_mut(8) {
                if matches!(p.as_str(), "MOLDING" | "SKIRTINGBOARD" | "CAISSON_FOUNDATION") {
                    *p = "NOTDEFINED".into();
                }
            }
        }
        "IFCSWEPTDISKSOLID" => sweep_range(facts, &mut a),
        "IFCBOUNDARYNODECONDITION" => {
            stiffness(&mut a);
            note(notes, STIFFNESS_NOTE);
        }
        "IFCSTRUCTURALCURVEMEMBER" => {
            member_axis(f, e, oh, id, &mut a);
            note(notes, AXIS_NOTE);
        }
        "IFCSTRUCTURALANALYSISMODEL" => {
            model_placement(f, e, oh, id, &mut a);
            note(notes, PLACEMENT_NOTE);
        }
        _ => {
            if let Some((new, n, pad)) = rule(class) {
                a.truncate(n);
                if pad {
                    a.push(Value::Null);
                }
                name = new.unwrap_or(name);
            }
        }
    }
    if class == "IFCPROPERTYSET" {
        if let (Some(add), Some(Value::List(l))) = (e.extend.get(&id), a.get_mut(4)) {
            l.extend(add.iter().map(|&p| Value::Ref(p)));
        }
    }
    // owner history on rooted entities
    if a.len() >= 4 && matches!(a.first(), Some(Value::Str(g)) if is_global_id(g)) {
        a[1] = Value::Ref(oh);
    }
    line(id, name, &a)
}

/// Every entity of `f` as IFC2X3 text appended to `out`; what the conversion needs besides
/// goes to `e`. `oh` is the owner history from [`owner_history`]; `notes` get what the
/// conversion changed in meaning.
pub(super) fn downgrade(
    f: &StepFile,
    e: &mut Emit,
    oh: u32,
    out: &mut String,
    notes: &mut Vec<String>,
) {
    let mut facts = Facts::default();
    for (id, t) in f.iter() {
        if t == "IFCTRIANGULATEDFACESET" {
            let closed = matches!(f.args(id).get(2), Some(Value::Enum(c)) if c == "T");
            facts.closed.insert(id, closed);
        }
        if t == "IFCPOLYGONALFACESET" {
            let closed = matches!(f.args(id).get(1), Some(Value::Enum(c)) if c == "T");
            facts.closed.insert(id, closed);
        }
    }
    advanced(f, &mut facts);
    for (id, class) in f.iter() {
        if dropped(class) || e.dropped.contains(&id) || facts.gone.contains(&id) {
            continue;
        }
        let line = convert(f, e, &facts, oh, id, notes);
        if class == "IFCINDEXEDPOLYCURVE" {
            // points of the polyline, the leading `#id` not counted
            let points = line.matches('#').count().saturating_sub(1);
            facts.ends.insert(id, points.saturating_sub(1) as f64);
        }
        out.push_str(&line);
        out.push('\n');
    }
}
