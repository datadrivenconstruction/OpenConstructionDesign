//! Component (CM) / equipment (ME): any family — from the project (loaded, modified,
//! programmatic, imported), the built-in library or the user's folder; click =
//! insert, Space = rotate 90°. The host follows the family's base: wall — nearest wall,
//! floor / ceiling — the one whose outline contains the point, face — wall nearby, else floor,
//! else ceiling. Dragging a card from the library places via the same path (`place`).

use std::sync::Arc;

use super::{Active, Rec};
use crate::editor::Editor;
use ddc_geom::component::Hosting;
use ddc_geom::{v2, V2};
use ddc_model::{CompHost, CompSpec};

/// A family type picked in the component library: dragged onto a view or armed in the tool.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    pub family: String,
    pub ty: String,
    /// Definition text for a family the project does not have yet.
    pub def: Option<Arc<str>>,
}

impl Pick {
    pub fn label(&self) -> String {
        format!("{} : {}", self.family, self.ty)
    }
}

/// Host hint from the view under the drop point (3D: the element hit).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HostHint {
    /// Find the host in plan by the family's hosting.
    Auto,
    Wall(i64),
    Slab(i64),
    Ceiling(i64),
}

/// "Family : Type" of every family the tool can place.
pub fn type_labels(ed: &Editor) -> Vec<String> {
    crate::complib::type_labels(&ed.model)
}


/// Wall nearest to `q` on the active level: (id, distance along its axis, distance to it).
fn nearest_wall(ed: &Editor, q: V2) -> Option<(i64, f64, f64)> {
    ed.walls
        .iter()
        .map(|w| (w, crate::snap::closest_on_segment(q, w.a, w.b)))
        .min_by(|x, y| (x.1 .0 - q).len().total_cmp(&(y.1 .0 - q).len()))
        .map(|(w, (f, _))| (w.id, (f - w.a).len(), (f - q).len()))
}

fn inside(q: V2, ring: &[V2]) -> bool {
    super::layer::inside(q, ring)
}

/// Floor of the active level whose outline holds `q` (outside its holes).
fn slab_at(ed: &Editor, q: V2) -> Option<i64> {
    ed.slabs
        .iter()
        .find(|s| {
            s.rings.first().is_some_and(|o| inside(q, o))
                && !s.rings[1..].iter().chain(s.cuts.iter()).any(|h| inside(q, h))
        })
        .map(|s| s.id)
}

/// Ceiling of level `level` whose outline holds `q`.
fn ceiling_at(ed: &Editor, level: i64, q: V2) -> Option<i64> {
    let mut st = ed
        .model
        .sql()
        .prepare_cached(
            "SELECT v.ceiling, v.x, v.y FROM ceiling_vertex v JOIN ceiling c ON c.id=v.ceiling \
             WHERE c.level=?1 AND v.ring=0 ORDER BY v.ceiling, v.idx",
        )
        .ok()?;
    let rows: Vec<(i64, f64, f64)> = st
        .query_map([level], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .ok()?
        .filter_map(Result::ok)
        .collect();
    let mut rings: Vec<(i64, Vec<V2>)> = vec![];
    for (id, x, y) in rows {
        match rings.last_mut() {
            Some((c, r)) if *c == id => r.push(v2(x, y)),
            _ => rings.push((id, vec![v2(x, y)])),
        }
    }
    rings.into_iter().find(|(_, r)| inside(q, r)).map(|(id, _)| id)
}

/// How close (m) a face-hosted family must be to a wall to sit on it.
const FACE_WALL: f64 = 0.3;

/// Instance at plan point `q` of the active level: the host by the family's hosting (or the
/// element `hint` names), rotation `rotation`. Error: catalog key of the reason.
pub fn spec_for(
    ed: &Editor,
    level: i64,
    rotation: f64,
    q: V2,
    fam: &str,
    hint: HostHint,
) -> Result<CompSpec, &'static str> {
    spec_for_def(ed, level, rotation, q, fam, hint, None)
}

/// [`spec_for`] for a family the project may not have yet: hosting then comes from `def`.
pub fn spec_for_def(
    ed: &Editor,
    level: i64,
    rotation: f64,
    q: V2,
    fam: &str,
    hint: HostHint,
    def: Option<&str>,
) -> Result<CompSpec, &'static str> {
    let mut s = CompSpec::at(level, 0, q.x, q.y);
    s.rotation = rotation;
    let on_wall = |s: &mut CompSpec, id: i64| -> Result<(), &'static str> {
        let w = ed.walls.iter().find(|w| w.id == id).ok_or("tool.err.no_host")?;
        let (f, _) = crate::snap::closest_on_segment(q, w.a, w.b);
        s.host = CompHost::Wall(id);
        s.at = ((f - w.a).len(), 0.0, 0.0);
        s.rotation = 0.0;
        Ok(())
    };
    let h = match crate::complib::family_of(&ed.model, fam) {
        Some(f) => f.hosting,
        None => def
            .and_then(|d| ddc_geom::component::Family::parse(d).ok())
            .map_or(Hosting::None, |f| f.hosting),
    };
    match (h, hint) {
        (Hosting::Wall | Hosting::Face, HostHint::Wall(id)) => on_wall(&mut s, id)?,
        (Hosting::Wall, _) => {
            let (id, _, _) = nearest_wall(ed, q).ok_or("tool.err.no_host")?;
            on_wall(&mut s, id)?;
        }
        (Hosting::Floor | Hosting::Face, HostHint::Slab(id)) => s.host = CompHost::Floor(id),
        (Hosting::Ceiling | Hosting::Face, HostHint::Ceiling(id)) => {
            s.host = CompHost::Ceiling(id)
        }
        (Hosting::Floor, _) => {
            if let Some(id) = slab_at(ed, q) {
                s.host = CompHost::Floor(id);
            }
        }
        (Hosting::Ceiling, _) => {
            if let Some(id) = ceiling_at(ed, level, q) {
                s.host = CompHost::Ceiling(id);
            }
        }
        (Hosting::Face, _) => match nearest_wall(ed, q) {
            Some((id, _, d)) if d <= FACE_WALL => on_wall(&mut s, id)?,
            _ => {
                if let Some(id) = slab_at(ed, q) {
                    s.host = CompHost::Floor(id);
                } else if let Some(id) = ceiling_at(ed, level, q) {
                    s.host = CompHost::Ceiling(id);
                }
            }
        },
        (Hosting::None, _) => {}
    }
    Ok(s)
}

/// Instance placement at a plan point, given the family and host.
pub fn spec_at(ed: &Editor, a: &Active, q: V2, fam: &str) -> Result<CompSpec, String> {
    spec_for(ed, a.level, a.rotation(), q, fam, HostHint::Auto).map_err(|k| a.t(k))
}

/// The family is loaded when the project does not have it (from `def`, else the built-in
/// library or the user folder), then the instance of type `ty` is added at `s`. Model ops made.
fn add_to(
    m: &mut ddc_model::Model,
    fam: &str,
    ty: &str,
    def: Option<&str>,
    s: &CompSpec,
) -> Result<usize, ddc_model::Error> {
    let had = m.family_id(fam)?;
    let fid = match had {
        Some(id) => id,
        None => {
            let d = match def {
                Some(d) => d.to_string(),
                None => crate::complib::def_of(m, fam)
                    .ok_or_else(|| ddc_model::Error::Invalid(format!("no family {fam}")))?,
            };
            m.load_family(&d)?
        }
    };
    let tid = m
        .comp_type_id(fid, ty)?
        .ok_or_else(|| ddc_model::Error::Invalid(format!("no type {ty}")))?;
    m.add_component(&CompSpec {
        ctype: tid,
        ..s.clone()
    })?;
    Ok(if had.is_some() { 1 } else { 2 })
}

fn add_op(fam: String, ty: String, def: Option<Arc<str>>, s: CompSpec) -> Rec {
    Rec::new("component", move |m| add_to(m, &fam, &ty, def.as_deref(), &s))
}

pub fn rec(ed: &Editor, a: &Active, q: V2) -> Result<Rec, String> {
    let label = a.type_name().to_string();
    let (fam, ty) = label
        .split_once(" : ")
        .map(|(f, t)| (f.to_string(), t.to_string()))
        .ok_or_else(|| a.t("tool.err.no_library"))?;
    let s = spec_at(ed, a, q, &fam)?;
    Ok(add_op(fam, ty, None, s))
}

/// Places `pick` at plan point `q` of the active level (a drop from the library), one undo step.
/// Err: the reason in the UI language.
pub fn place(ed: &mut Editor, pick: &Pick, q: V2, hint: HostHint) -> Result<(), String> {
    let s = spec_for_def(ed, ed.level, 0.0, q, &pick.family, hint, pick.def.as_deref()).map_err(|k| crate::tr(k, &[]))?;
    let p = pick.clone();
    let op = crate::editor::OpFn(Arc::new(move |m: &mut ddc_model::Model| {
        add_to(m, &p.family, &p.ty, p.def.as_deref(), &s).map(|_| ())
    }));
    ed.run_op(op).map_err(|e| e.to_string())
}

/// Arms the Component tool with `pick` (double-click in the library).
pub fn arm_with(ed: &mut Editor, pick: &Pick, lang: ddc_ui::Lang) {
    super::arm(ed, super::Kind::Component, lang);
    let want = pick.label();
    if let Some(a) = ed.model_tool_mut() {
        if !a.types.contains(&want) {
            a.types.insert(0, want.clone());
        }
        a.opts.ty = a.types.iter().position(|t| *t == want).unwrap_or(0);
    }
}
