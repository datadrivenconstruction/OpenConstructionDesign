// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Building elements and fasteners from Lua. `wall`, `door`, `window`, `room` and `roof` create
//! the model's own elements (not solids), so they count, schedule and export as such. `plate`,
//! `bolt` and `rivet` are solid parts with the right IFC class.

use ddc_ops::{Angle, Cat, End, Field, Len, NewEl, Op, Place, Ref, Side, Unit, Value as OpValue, P3};
use mlua::{Lua, Table, UserData, Value};

use crate::blocks::{block, Em, Opts};
use crate::emit::Builder;
use crate::stdlib::builder;
use crate::values::{err, GroupH, LResult, V3};

/// A created building element: what a `door` or `window` names as its wall.
#[derive(Clone, Copy)]
pub(crate) struct ElH {
    slot: u32,
    cat: Cat,
}

impl UserData for ElH {
    fn add_methods<M: mlua::UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_method(mlua::MetaMethod::ToString, |_, s, ()| Ok(format!("{:?} #{}", s.cat, s.slot)));
    }
}

impl Builder {
    /// One new building element; returns its slot.
    pub(crate) fn element(&mut self, el: NewEl) -> Result<u32, String> {
        self.count_element()?;
        let slot = self.ops.len() as u32;
        self.ops.push(Op::Create { slot, el });
        Ok(slot)
    }
}

fn at(p: V3) -> Place {
    Place::At { p: P3::m(p.x, p.y, p.z) }
}

fn level(lua: &Lua, f: &str) -> LResult<Ref> {
    builder(lua)?
        .level
        .clone()
        .ok_or_else(|| mlua::Error::RuntimeError(format!("{f}: the run has no level")))
}

fn type_ref(o: &Opts, cat: Cat) -> LResult<Option<Ref>> {
    let n = o.text("type", "")?;
    Ok((!n.is_empty()).then(|| Ref::name(cat, &n)))
}

fn make(lua: &Lua, f: &str, cat: Cat, el: NewEl) -> LResult<ElH> {
    let slot = builder(lua)?
        .element(el)
        .map_err(|e| mlua::Error::RuntimeError(format!("{f}: {e}")))?;
    Ok(ElH { slot, cat })
}

fn m_(name: &str, v: f64) -> Field {
    Field::new(name, OpValue::num(v, Unit::M))
}

fn wall(lua: &Lua, t: Option<Table>) -> LResult<ElH> {
    let o = Opts::new("wall", t, lua)?;
    let (Some(a), Some(b)) = (o.opt_point("from")?, o.opt_point("to")?) else {
        return err("wall: needs from and to, e.g. wall{from = vec3(0, 0, 0), to = vec3(6, 0, 0), height = 3}");
    };
    let h = o.len_in("height", 3.0, 0.1, 1000.0)?;
    let ty = type_ref(&o, Cat::WallType)?;
    o.done()?;
    if b.sub(a).len() < 0.01 {
        return err("wall: from and to are the same point");
    }
    let level = level(lua, "wall")?;
    make(lua, "wall", Cat::Wall, NewEl::Wall { level, a: at(a), b: at(b), height: Len::m(h), top: None, ty, fields: vec![] })
}

/// `door` and `window` on a wall made by `wall{}`, centred `at` metres from the wall's start.
fn opening(lua: &Lua, t: Option<Table>, door: bool) -> LResult<ElH> {
    let f: &'static str = if door { "door" } else { "window" };
    let o = Opts::new(f, t, lua)?;
    let host = match o.value("wall")? {
        Value::UserData(u) if u.is::<ElH>() && u.borrow::<ElH>()?.cat == Cat::Wall => *u.borrow::<ElH>()?,
        _ => return err(format!("{f}: wall = the value returned by wall{{...}}")),
    };
    let along = o.len("at", 0.0)?;
    let w = o.len_in("width", if door { 0.9 } else { 1.2 }, 0.05, 20.0)?;
    let h = o.len_in("height", if door { 2.1 } else { 1.2 }, 0.05, 20.0)?;
    let sill = o.len_in("sill", if door { 0.0 } else { 0.9 }, 0.0, 20.0)?;
    o.done()?;
    let hr = Ref::slot(host.slot);
    let place = Place::OnHost { host: hr.clone(), along: Len::m(along), from_end: End::A, side: Side::Center, z: Len::m(sill) };
    let fields = vec![m_("width", w), m_("height", h)];
    let el = if door {
        NewEl::Door { host: hr, at: Place::OnHost { host: Ref::slot(host.slot), along: Len::m(along), from_end: End::A, side: Side::Center, z: Len::m(0.0) }, ty: None, fields: [fields, vec![m_("sill", sill)]].concat() }
    } else {
        NewEl::Window { host: hr, at: place, sill: Len::m(sill), ty: None, fields }
    };
    make(lua, f, if door { Cat::Door } else { Cat::Window }, el)
}

fn room(lua: &Lua, t: Option<Table>) -> LResult<ElH> {
    let o = Opts::new("room", t, lua)?;
    let Some(p) = o.opt_point("at")? else {
        return err("room: needs a point inside it, e.g. room{at = vec3(2, 2, 0), name = \"Hall\"}");
    };
    let name = o.text("name", "Room")?;
    let number = o.text("number", "")?;
    o.done()?;
    let level = level(lua, "room")?;
    make(lua, "room", Cat::Room, NewEl::Room { level, at: at(p), name, number: (!number.is_empty()).then_some(number) })
}

/// `place_family{family, at, rotation, host, type}`: one loaded family (furniture, fixture) as a
/// component element at a point.
fn place_family(lua: &Lua, t: Option<Table>) -> LResult<ElH> {
    let o = Opts::new("place_family", t, lua)?;
    let family = o.text("family", "")?;
    let (Some(p), false) = (o.opt_point("at")?, family.is_empty()) else {
        return err("place_family: needs family and at, e.g. place_family{family = \"Chair\", at = vec3(1, 2, 0)}");
    };
    let rotation = o.ang("rotation", 0.0)?;
    let host = match o.value("host")? {
        Value::Nil => None,
        Value::UserData(u) if u.is::<ElH>() => Some(Ref::slot(u.borrow::<ElH>()?.slot)),
        _ => return err("place_family: host = the value returned by wall{...}"),
    };
    let ty = type_ref(&o, Cat::Family)?;
    o.done()?;
    let level = level(lua, "place_family")?;
    let family = Ref::name(Cat::Family, &family);
    make(lua, "place_family", Cat::Component, NewEl::Component { level, at: at(p), family, host, rotation: Angle::rad(rotation), ty, fields: vec![] })
}

/// `pipe{system, path, size}`: a run of pipe (or duct, cable tray) along a path, as a MEP element.
fn pipe(lua: &Lua, t: Option<Table>) -> LResult<ElH> {
    let o = Opts::new("pipe", t, lua)?;
    let system = o.text("system", "")?;
    let Some(pts) = o.points("path")?.filter(|p| p.len() >= 2) else {
        return err("pipe: needs a path of two or more points, e.g. pipe{system = \"Cold water\", path = {vec3(0, 0, 0), vec3(3, 0, 0)}, size = \"20 mm\"}");
    };
    if system.is_empty() {
        return err("pipe: needs the system it belongs to, e.g. system = \"Cold water\"");
    }
    let size = o.len_in("size", 0.05, 0.001, 5.0)?;
    let ty = type_ref(&o, Cat::MepRun)?;
    o.done()?;
    let level = level(lua, "pipe")?;
    make(lua, "pipe", Cat::MepRun, NewEl::MepRun { level, system, path: pts.into_iter().map(at).collect(), size: Len::m(size), ty, fields: vec![] })
}

/// `roof{outline, slope, overhang, gables = {edge numbers}, type}`: a roof element over a plan.
fn roof(lua: &Lua, t: Option<Table>) -> LResult<ElH> {
    let o = Opts::new("roof", t, lua)?;
    let Some(pts) = o.points("outline")?.filter(|p| p.len() >= 3) else {
        return err("roof: needs an outline of three or more points, e.g. roof{outline = {vec3(0, 0, 0), vec3(8, 0, 0), vec3(8, 6, 0), vec3(0, 6, 0)}, slope = \"35 deg\"}");
    };
    let slope = o.ang_in("slope", 30f64.to_radians(), 0.0, 89.0)?;
    let overhang = o.len_in("overhang", 0.0, 0.0, 20.0)?;
    let offset = o.len("offset", 0.0)?;
    let ty = type_ref(&o, Cat::SlabType)?;
    let mut gables = vec![];
    if let Some(g) = o.table("gables")? {
        for x in g.sequence_values::<i64>() {
            gables.push(x?);
        }
    }
    o.done()?;
    let mut fields = vec![m_("overhang", overhang), m_("offset", offset)];
    if !gables.is_empty() {
        fields.push(Field::new("gables", OpValue::plain(serde_json::json!(gables))));
    }
    let level = level(lua, "roof")?;
    make(
        lua,
        "roof",
        Cat::Roof,
        NewEl::Roof { level, boundary: pts.into_iter().map(at).collect(), slope: Angle::rad(slope), ty, fields },
    )
}

/// `plate{outline, thickness, material, name}`: a flat plate of any orientation.
fn plate(lua: &Lua, t: Option<Table>) -> LResult<(GroupH, Table)> {
    let o = Opts::new("plate", t, lua)?;
    let Some(pts) = o.points("outline")?.filter(|p| p.len() >= 3) else {
        return err("plate: needs an outline of three or more points, e.g. plate{outline = {vec3(0,0,0), vec3(0.2,0,0), vec3(0.2,0.3,0)}, thickness = \"12 mm\"}");
    };
    let th = o.len_in("thickness", 0.012, 0.001, 1.0)?;
    let mat = o.mat("material", "S355")?;
    let name = o.text("name", "plate")?;
    let role = o.choice("role", "plate", &["plate", "panel", "gusset", "base_plate", "tread"])?;
    o.done()?;
    let mut em = Em::new(lua, "plate");
    if em.panel(&role, &pts, th, &mat)? == 0 {
        return err("plate: the outline has no area");
    }
    em.finish(&name)
}

/// `bolt{at, dir, grip, diameter}` and `rivet{...}`: one fastener along `dir` through `grip`.
fn fastener(lua: &Lua, t: Option<Table>, rivet: bool) -> LResult<(GroupH, Table)> {
    let f: &'static str = if rivet { "rivet" } else { "bolt" };
    let o = Opts::new(f, t, lua)?;
    let Some(p) = o.opt_point("at")? else {
        return err(format!("{f}: needs at = vec3(...), e.g. {f}{{at = vec3(0, 0, 0), dir = vec3(0, 0, 1), grip = \"20 mm\", diameter = \"16 mm\"}}"));
    };
    let dir = o.point("dir", V3::new(0.0, 0.0, 1.0))?;
    let grip = o.len_in("grip", 0.02, 0.001, 5.0)?;
    let d = o.len_in("diameter", if rivet { 0.016 } else { 0.020 }, 0.002, 0.2)?;
    let mat = o.mat("material", "S355")?;
    o.done()?;
    let Some(n) = dir.unit() else { return err(format!("{f}: dir is a zero vector")) };
    let mut em = Em::new(lua, f);
    if rivet {
        let rod = ddc_ops::Profile::Circle { r: Len::m(d / 2.0) };
        em.member("rivet", &rod, p.sub(n.scale(0.4 * d)), p.add(n.scale(grip + 0.4 * d)), &mat)?;
    } else {
        em.bolt(p, n, grip, d, &mat)?;
    }
    em.finish(f)
}

/// `lattice_girder{from, to, depth, panels, chord, web, up}`: a Warren girder with posts, its
/// chords along `from`-`to` and its depth towards `up` (default vertical).
fn lattice_girder(lua: &Lua, t: Option<Table>) -> LResult<(GroupH, Table)> {
    let o = Opts::new("lattice_girder", t, lua)?;
    let (Some(a), Some(b)) = (o.opt_point("from")?, o.opt_point("to")?) else {
        return err("lattice_girder: needs from and to, e.g. lattice_girder{from = vec3(0, 0, 6), to = vec3(12, 0, 6), depth = 1.2, panels = 8}");
    };
    let depth = o.len_in("depth", 1.0, 0.05, 50.0)?;
    let n = o.int("panels", 8, 1, 400)? as usize;
    let up = o.point("up", V3::new(0.0, 0.0, 1.0))?;
    let chord = o.prof("chord", "2L 80x80x8")?;
    let web = o.prof("web", "2L 50x50x5")?;
    let mat = o.mat("material", "S355")?;
    let name = o.text("name", "lattice girder")?;
    o.done()?;
    let axis = b.sub(a);
    let Some(ax) = axis.unit() else { return err("lattice_girder: from and to are the same point") };
    let Some(u) = up.sub(ax.scale(up.dot(ax))).unit() else {
        return err("lattice_girder: up must not be along the girder");
    };
    let low = |i: usize| a.add(axis.scale(i as f64 / n as f64));
    let high = |i: usize| low(i).add(u.scale(depth));
    let mut em = Em::new(lua, "lattice_girder");
    for i in 0..n {
        em.member("chord", &chord, low(i), low(i + 1), &mat)?;
        em.member("chord", &chord, high(i), high(i + 1), &mat)?;
        let (p, q) = if i % 2 == 0 { (low(i), high(i + 1)) } else { (high(i), low(i + 1)) };
        em.member("diagonal", &web, p, q, &mat)?;
    }
    for i in 0..=n {
        em.member("post", &web, low(i), high(i), &mat)?;
    }
    em.finish(&name)
}

pub(crate) fn install(lua: &Lua, g: &Table) -> LResult<()> {
    g.set("lattice_girder", block(lua, lattice_girder)?)?;
    g.set("wall", lua.create_function(wall)?)?;
    g.set("door", lua.create_function(|lua, t: Option<Table>| opening(lua, t, true))?)?;
    g.set("window", lua.create_function(|lua, t: Option<Table>| opening(lua, t, false))?)?;
    g.set("room", lua.create_function(room)?)?;
    g.set("roof", lua.create_function(roof)?)?;
    g.set("place_family", lua.create_function(place_family)?)?;
    g.set("pipe", lua.create_function(pipe)?)?;
    g.set("plate", block(lua, plate)?)?;
    g.set("bolt", block(lua, |l, t| fastener(l, t, false))?)?;
    g.set("rivet", block(lua, |l, t| fastener(l, t, true))?)?;
    Ok(())
}
