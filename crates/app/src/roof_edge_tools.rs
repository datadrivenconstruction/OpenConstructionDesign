// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Roof and edge commands: roof presets, vault, roof attach, fascia board, gutter,
//! soffit, skylight, slab edge, wall molding/cornice/rustication, wedge to drains.
//! Each action is one `Editor::run_op` operation (one undo step). Sizes come from the session
//! parameters ([`Params`], the `roof_edge_ui` window). Plan clicks for the vault, skylight, attach and
//! drains go through `handle`; a pending command is cleared by a tool/level change, undo, opening a
//! file, and any command of this module (`reset`).

use std::cell::RefCell;
use std::sync::Arc;

use ddc_geom::roof_shape::Preset;
use ddc_geom::sweep::SweepKind;
use ddc_geom::{v2, V2};
use ddc_model::{Error, Model, SkylightSpec, SweepHost, SweepSpec};
use ddc_render::{SceneMesh, TAG_GLASS, TAG_SLAB, TAG_WINDOW};

use crate::editor::levels::OTHER_LEVEL;
use crate::editor::{Editor, OpFn, Sel};
use crate::roof_edge_ui as dlg;
use crate::shell::EditorApp;
use crate::status::Severity;
use crate::wall_tool::{Event, Key, Mode};

// ---------------------------------------------------------------- commands and parameters

/// Command with parameters (a parameters window before creating/arming).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    Barrel,
    Skylight,
    Fascia,
    Gutter,
    Soffit,
    SlabEdge,
    WallSweep,
    Cornice,
    Reveal,
    Taper,
}

impl Cmd {
    pub const ALL: [Cmd; 10] = [
        Cmd::Barrel,
        Cmd::Skylight,
        Cmd::Fascia,
        Cmd::Gutter,
        Cmd::Soffit,
        Cmd::SlabEdge,
        Cmd::WallSweep,
        Cmd::Cornice,
        Cmd::Reveal,
        Cmd::Taper,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Cmd::Barrel => "arch.roof_barrel",
            Cmd::Skylight => "arch.skylight",
            Cmd::Fascia => "arch.fascia",
            Cmd::Gutter => "arch.gutter",
            Cmd::Soffit => "arch.soffit",
            Cmd::SlabEdge => "str.slab_edge",
            Cmd::WallSweep => "arch.wall_sweep",
            Cmd::Cornice => "arch.wall_cornice",
            Cmd::Reveal => "arch.wall_reveal",
            Cmd::Taper => "arch.roof_taper",
        }
    }

    /// [`Params::sweeps`] slot for profile commands.
    pub fn sweep_slot(self) -> Option<usize> {
        Some(match self {
            Cmd::Fascia => 0,
            Cmd::Gutter => 1,
            Cmd::SlabEdge => 2,
            Cmd::WallSweep => 3,
            Cmd::Cornice => 4,
            Cmd::Reveal => 5,
            _ => return None,
        })
    }

    pub fn sweep_kind(self) -> Option<SweepKind> {
        Some(match self {
            Cmd::Fascia => SweepKind::Fascia,
            Cmd::Gutter => SweepKind::Gutter,
            Cmd::SlabEdge => SweepKind::SlabEdge,
            Cmd::WallSweep | Cmd::Cornice => SweepKind::WallSweep,
            Cmd::Reveal => SweepKind::Reveal,
            _ => return None,
        })
    }

    pub fn on_walls(self) -> bool {
        matches!(self, Cmd::WallSweep | Cmd::Cornice | Cmd::Reveal)
    }
}

/// Parameters of a profile command.
#[derive(Clone, Debug, PartialEq)]
pub struct SweepParams {
    pub profile: String,
    /// Path elevation above the wall bottom / height offset; for a cornice, the distance down from the wall top.
    pub z: f64,
    pub dx: f64,
    pub dy: f64,
    /// Wall face: -1 right, +1 left along the chain direction.
    pub side: i8,
    /// Roof: also along the gable overhangs.
    pub rakes: bool,
}

fn sp(profile: &str, z: f64, rakes: bool) -> SweepParams {
    SweepParams {
        profile: profile.into(),
        z,
        dx: 0.0,
        dy: 0.0,
        side: -1,
        rakes,
    }
}

/// Per-session command parameters (defaults are the former constants), meters and m/m.
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    /// Skylight: width across the slope, length along the slope, curb height.
    pub sky: (f64, f64, f64),
    pub barrel_offset: f64,
    /// Vault rise as a fraction of the span (the model allows <= 0.4).
    pub barrel_rise: f64,
    /// Vault covering type; None or a deleted one falls back to the file's first type.
    pub slab_type: Option<i64>,
    /// Wedge: slope, m/m; minimum thickness.
    pub taper: (f64, f64),
    /// Soffit: drop, thickness.
    pub soffit: (f64, f64),
    /// Indexed by [`Cmd::sweep_slot`].
    pub sweeps: [SweepParams; 6],
}

impl Default for Params {
    fn default() -> Self {
        Params {
            sky: (0.78, 1.18, 0.15),
            barrel_offset: 3.0,
            barrel_rise: 0.25,
            slab_type: None,
            taper: (0.02, 0.05),
            soffit: (0.0, 0.02),
            sweeps: [
                sp("fascia_200", 0.0, true),
                sp("gutter_half_round_150", 0.0, false),
                sp("slab_edge_200x450", 0.0, false),
                sp("base_100x20", 0.0, false),
                sp("cornice_ovolo", 0.16, false),
                sp("reveal_20x20", 1.0, false),
            ],
        }
    }
}

thread_local! {
    static PARAMS: RefCell<Params> = RefCell::new(Params::default());
}

pub fn params() -> Params {
    PARAMS.with(|p| p.borrow().clone())
}

pub fn set_params(p: Params) {
    PARAMS.with(|s| *s.borrow_mut() = p);
}

/// Edge element (3D pick, properties, deletion).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Elem {
    Sweep(i64),
    Skylight(i64),
    Soffit(i64),
    Taper(i64),
}

impl Elem {
    /// Kind for `Model::delete_roof_edge`.
    pub fn kind(self) -> &'static str {
        match self {
            Elem::Sweep(_) => "sweep",
            Elem::Skylight(_) => "skylight",
            Elem::Soffit(_) => "soffit",
            Elem::Taper(_) => "taper",
        }
    }

    pub fn id(self) -> i64 {
        match self {
            Elem::Sweep(i) | Elem::Skylight(i) | Elem::Soffit(i) | Elem::Taper(i) => i,
        }
    }

    pub fn index(self) -> u32 {
        match self {
            Elem::Sweep(_) => 0,
            Elem::Skylight(_) => 1,
            Elem::Soffit(_) => 2,
            Elem::Taper(_) => 3,
        }
    }

    pub fn from_index(i: u32, id: i64) -> Option<Elem> {
        Some(match i {
            0 => Elem::Sweep(id),
            1 => Elem::Skylight(id),
            2 => Elem::Soffit(id),
            3 => Elem::Taper(id),
            _ => return None,
        })
    }

    /// Body kind of `Model::roof_edge_bodies` -> element (skylight glass maps to the skylight itself).
    pub fn from_kind(kind: &str, id: i64) -> Option<Elem> {
        Some(match kind {
            "sweep" => Elem::Sweep(id),
            "skylight" | "skylight_glass" => Elem::Skylight(id),
            "soffit" => Elem::Soffit(id),
            "taper" => Elem::Taper(id),
            _ => return None,
        })
    }
}

/// Palette tag of an edge body.
pub fn tag_of(kind: &str) -> u32 {
    match kind {
        "skylight" => TAG_WINDOW,
        "skylight_glass" => TAG_GLASS,
        _ => TAG_SLAB,
    }
}

// ---------------------------------------------------------------- waiting for a click

enum Pending {
    /// Vault: span start, end, side/depth.
    Barrel(Vec<V2>),
    /// Attach roof `0` to the roof under the click.
    Join(i64),
    Skylight,
    /// Drains of wedge `0`.
    Drains(i64),
}

struct State {
    p: Pending,
    /// Level the command was armed on.
    level: i64,
}

thread_local! {
    static PENDING: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Clear the pending command (tool/level change, undo, file open).
pub fn reset() {
    PENDING.with(|s| *s.borrow_mut() = None);
}

pub fn armed() -> bool {
    PENDING.with(|s| s.borrow().is_some())
}

/// Command of the pending pick, while `handle` would take it (`interact::Scope`).
pub fn armed_command(ed: &Editor) -> Option<&'static str> {
    PENDING.with(|s| {
        let s = s.borrow();
        let st = s.as_ref().filter(|st| alive(ed, st))?;
        Some(match st.p {
            Pending::Barrel(_) => "arch.roof_barrel",
            Pending::Join(_) => "arch.roof_join",
            Pending::Skylight => "arch.skylight",
            Pending::Drains(_) => "arch.roof_taper",
        })
    })
}

fn t(key: &str) -> String {
    crate::tr(key, &[])
}

fn arm(ed: &mut Editor, p: Pending, hint: &str) {
    ed.set_mode(Mode::Select);
    ed.say(Severity::Info, t(hint));
    PENDING.with(|s| *s.borrow_mut() = Some(State { p, level: ed.level }));
}

pub(crate) fn run(
    ed: &mut Editor,
    ok: &str,
    f: impl Fn(&mut Model) -> Result<(), Error> + Send + Sync + 'static,
) -> bool {
    match ed.run_op(OpFn(Arc::new(f))) {
        Ok(()) => {
            ed.say(Severity::Info, t(ok));
            true
        }
        Err(e) => {
            ed.say(Severity::Error, format!("{}: {e}", t("roof_edge.failed")));
            false
        }
    }
}

fn level_roofs(ed: &Editor) -> Vec<i64> {
    let db = ed.model.sql();
    let Ok(mut st) = db.prepare("SELECT id FROM roof WHERE level=?1 ORDER BY id") else {
        return vec![];
    };
    st.query_map([ed.level], |r| r.get(0))
        .map(|rs| rs.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// Covering types of the file (id, name).
pub(crate) fn slab_types(m: &Model) -> Vec<(i64, String)> {
    let Ok(mut st) = m
        .sql()
        .prepare("SELECT id, name FROM slab_type ORDER BY id")
    else {
        return vec![];
    };
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|rs| rs.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

fn slab_type(m: &Model, pref: Option<i64>) -> Option<i64> {
    let ts = slab_types(m);
    pref.filter(|id| ts.iter().any(|t| t.0 == *id))
        .or_else(|| ts.first().map(|t| t.0))
}

/// Roof of the action: the one in handle focus, otherwise the last on the level.
fn target_roof(ed: &Editor) -> Option<i64> {
    ed.grip
        .focus
        .iter()
        .find(|f| f.0 == crate::grips::Kind::Roof)
        .map(|f| f.1)
        .or_else(|| level_roofs(ed).into_iter().max())
}

/// Topmost roof of the level above a point (except `skip`).
fn roof_at(ed: &Editor, p: V2, skip: Option<i64>) -> Option<i64> {
    level_roofs(ed)
        .into_iter()
        .filter(|&id| Some(id) != skip)
        .filter_map(|id| {
            let z = ed.model.roof_geometry(id).ok()?.top_at(p)?;
            Some((id, z))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|r| r.0)
}

fn selected_walls(ed: &Editor) -> Vec<i64> {
    ed.selection
        .iter()
        .filter_map(|s| match s {
            Sel::Wall(w) => Some(*w),
            _ => None,
        })
        .collect()
}

fn selected_slab(ed: &Editor) -> Option<i64> {
    ed.selection.iter().find_map(|s| match s {
        Sel::Slab(id) => Some(*id),
        _ => None,
    })
}

fn need_roof(ed: &mut Editor) -> Option<i64> {
    let r = target_roof(ed);
    if r.is_none() {
        ed.say(Severity::Refusal, t("roof_edge.need_roof"));
    }
    r
}

fn need_slab(ed: &mut Editor) -> Option<i64> {
    let s = selected_slab(ed);
    if s.is_none() {
        ed.say(Severity::Refusal, t("roof_edge.need_slab"));
    }
    s
}

/// Command preconditions before the parameters window (the status says what must be selected).
pub fn precheck(ed: &mut Editor, cmd: Cmd) -> bool {
    match cmd {
        Cmd::Fascia | Cmd::Gutter | Cmd::Soffit => need_roof(ed).is_some(),
        Cmd::SlabEdge | Cmd::Taper => need_slab(ed).is_some(),
        Cmd::WallSweep | Cmd::Cornice | Cmd::Reveal => {
            let ok = !selected_walls(ed).is_empty();
            if !ok {
                ed.say(Severity::Refusal, t("roof_edge.need_walls"));
            }
            ok
        }
        Cmd::Barrel => {
            let ok = slab_type(&ed.model, None).is_some();
            if !ok {
                ed.say(Severity::Refusal, t("roof_edge.need_type"));
            }
            ok
        }
        Cmd::Skylight => {
            let ok = !level_roofs(ed).is_empty();
            if !ok {
                ed.say(Severity::Refusal, t("roof_edge.need_roof"));
            }
            ok
        }
    }
}

/// Run a command with parameters: create the element or arm a click wait.
pub fn exec(ed: &mut Editor, cmd: Cmd, p: &Params) -> bool {
    match cmd {
        Cmd::Barrel => {
            arm(ed, Pending::Barrel(vec![]), "roof_edge.hint.barrel");
            true
        }
        Cmd::Skylight => {
            arm(ed, Pending::Skylight, "roof_edge.hint.skylight");
            true
        }
        Cmd::Soffit => {
            let Some(roof) = need_roof(ed) else {
                return false;
            };
            let (dh, th) = p.soffit;
            run(ed, "roof_edge.done", move |m| {
                m.add_soffit(roof, dh, th, None).map(|_| ())
            })
        }
        Cmd::Taper => taper(ed, p),
        _ => sweep_cmd(ed, cmd, p),
    }
}

fn wall_height(ed: &Editor, wall: i64) -> Option<f64> {
    ed.model
        .sql()
        .query_row("SELECT height FROM wall WHERE id=?1", [wall], |r| r.get(0))
        .ok()
}

fn sweep_cmd(ed: &mut Editor, cmd: Cmd, p: &Params) -> bool {
    let (Some(i), Some(kind)) = (cmd.sweep_slot(), cmd.sweep_kind()) else {
        return false;
    };
    let sp = p.sweeps[i].clone();
    let (host, z, side) = match cmd {
        Cmd::Fascia | Cmd::Gutter => {
            let Some(roof) = need_roof(ed) else {
                return false;
            };
            (
                SweepHost::Roof {
                    roof,
                    rakes: sp.rakes,
                },
                sp.z,
                -1,
            )
        }
        Cmd::SlabEdge => {
            let Some(slab) = need_slab(ed) else {
                return false;
            };
            (SweepHost::Slab(slab), sp.z, -1)
        }
        _ => {
            let walls = selected_walls(ed);
            let Some(&w0) = walls.first() else {
                ed.say(Severity::Refusal, t("roof_edge.need_walls"));
                return false;
            };
            // cornice: the section sits on the path, below the first wall's top by the given distance
            let z = if cmd == Cmd::Cornice {
                wall_height(ed, w0).unwrap_or(3.0) - sp.z
            } else {
                sp.z
            };
            (SweepHost::Walls(walls), z, sp.side)
        }
    };
    let s = SweepSpec {
        kind,
        profile: sp.profile,
        host,
        side,
        z,
        dx: sp.dx,
        dy: sp.dy,
        material: None,
    };
    run(ed, "roof_edge.done", move |m| m.add_sweep(&s).map(|_| ()))
}

fn taper(ed: &mut Editor, p: &Params) -> bool {
    let Some(slab) = need_slab(ed) else {
        return false;
    };
    let (slope, tmin) = p.taper;
    if !run(ed, "roof_edge.hint.drains", move |m| {
        m.add_taper_on_slab(slab, slope, tmin).map(|_| ())
    }) {
        return false;
    }
    let id: Option<i64> = ed
        .model
        .sql()
        .query_row("SELECT MAX(id) FROM roof_taper", [], |r| r.get(0))
        .ok()
        .flatten();
    if let Some(id) = id {
        arm(ed, Pending::Drains(id), "roof_edge.hint.drains");
    }
    true
}

/// Arm drain placement for an existing wedge (from the properties window).
pub fn arm_drains(ed: &mut Editor, taper: i64) {
    arm(ed, Pending::Drains(taper), "roof_edge.hint.drains");
}

/// Delete edge elements: one undo step; on failure, those already deleted are restored.
pub fn delete_elems(ed: &mut Editor, es: &[Elem]) -> bool {
    let mut items: Vec<Elem> = vec![];
    for e in es {
        if !items.contains(e) {
            items.push(*e);
        }
    }
    if items.is_empty() {
        return false;
    }
    let msg = match items.as_slice() {
        [e] => {
            let name = format!("{} #{}", t(&format!("roof_edge.k.{}", e.kind())), e.id());
            crate::tr("roof_edge.deleted_one", &[("name", &name)])
        }
        _ => crate::tr_n("roof_edge.deleted_n", items.len() as u64),
    };
    let rec = crate::tools::Rec::new("roof_edge_delete", move |m: &mut Model| {
        for (k, e) in items.iter().enumerate() {
            if let Err(err) = m.delete_roof_edge(e.kind(), e.id()) {
                for _ in 0..k {
                    let _ = m.undo();
                }
                return Err(err);
            }
        }
        Ok(items.len())
    });
    match ed.apply_ext(rec) {
        Ok(()) => {
            ed.say(Severity::Info, msg);
            ed.offer_undo();
            true
        }
        Err(e) => {
            ed.say(Severity::Error, format!("{}: {e}", t("roof_edge.failed")));
            false
        }
    }
}

// ---------------------------------------------------------------- registration

fn preset(app: &mut EditorApp, p: Preset) {
    let Some(roof) = need_roof(&mut app.ed) else {
        return;
    };
    let (slope, upper, brk) = match p {
        Preset::Mansard => (70.0, Some(25.0), 1.5),
        Preset::Gambrel => (60.0, Some(25.0), 1.5),
        _ => (30.0, None, 0.0),
    };
    run(&mut app.ed, "roof_edge.done", move |m| {
        m.apply_roof_preset(roof, p, slope, upper, brk)
    });
}

fn h_join(app: &mut EditorApp) {
    if let Some(roof) = need_roof(&mut app.ed) {
        arm(&mut app.ed, Pending::Join(roof), "roof_edge.hint.join");
    }
}

fn h_unjoin(app: &mut EditorApp) {
    if let Some(roof) = need_roof(&mut app.ed) {
        run(&mut app.ed, "roof_edge.done", move |m| m.unjoin_roof(roof));
    }
}

pub fn register(app: &mut EditorApp) {
    app.register_command("arch.roof_gable", |a| preset(a, Preset::Gable));
    app.register_command("arch.roof_hip", |a| preset(a, Preset::Hip));
    app.register_command("arch.roof_shed", |a| preset(a, Preset::Shed));
    app.register_command("arch.roof_mansard", |a| preset(a, Preset::Mansard));
    app.register_command("arch.roof_gambrel", |a| preset(a, Preset::Gambrel));
    app.register_command("arch.roof_join", h_join);
    app.register_command("arch.roof_unjoin", h_unjoin);
    app.register_command("arch.roof_barrel", |a| dlg::open_cmd(a, Cmd::Barrel));
    app.register_command("arch.skylight", |a| dlg::open_cmd(a, Cmd::Skylight));
    app.register_command("arch.fascia", |a| dlg::open_cmd(a, Cmd::Fascia));
    app.register_command("arch.gutter", |a| dlg::open_cmd(a, Cmd::Gutter));
    app.register_command("arch.soffit", |a| dlg::open_cmd(a, Cmd::Soffit));
    app.register_command("str.slab_edge", |a| dlg::open_cmd(a, Cmd::SlabEdge));
    app.register_command("arch.wall_sweep", |a| dlg::open_cmd(a, Cmd::WallSweep));
    app.register_command("arch.wall_cornice", |a| dlg::open_cmd(a, Cmd::Cornice));
    app.register_command("arch.wall_reveal", |a| dlg::open_cmd(a, Cmd::Reveal));
    app.register_command("arch.roof_taper", |a| dlg::open_cmd(a, Cmd::Taper));
    app.register_command("arch.roof_edges", dlg::open_list);
    app.register_command("arch.sweep_profiles", dlg::open_profiles);
    // 3D edge provider; "arch.roof_edges" shows the properties of the 3D selection, or a list
    crate::roof_edge_scene::install(app);
}

/// Another tool owns input: the edge wait is inactive.
fn foreign_tool(ed: &Editor) -> bool {
    ed.tool_armed()
        || ed.model_tool().is_some()
        || ed.opening_mode
        || ed.tool.curtain
        || ed.section_tool().is_some()
        || ed.sketch.is_some()
        || ed.shape.is_some()
}

/// The wait still applies to the model: same level, and the held element exists.
fn alive(ed: &Editor, st: &State) -> bool {
    st.level == ed.level
        && !foreign_tool(ed)
        && match st.p {
            Pending::Join(r) => ed.model.roof(r).is_ok(),
            Pending::Drains(k) => ed.model.taper(k).is_ok(),
            Pending::Barrel(_) | Pending::Skylight => true,
        }
}

/// Plan click for the pending command; false means there is no command or the event is not its.
pub fn handle(ed: &mut Editor, e: Event) -> bool {
    let Some(mut st) = PENDING.with(|s| s.borrow_mut().take()) else {
        return false;
    };
    if !alive(ed, &st) {
        return false;
    }
    let keep = match e {
        Event::Click(p) => click(ed, &mut st, p),
        Event::Key(Key::Escape) | Event::Key(Key::Enter) => {
            ed.status = None;
            false
        }
        _ => {
            PENDING.with(|s| *s.borrow_mut() = Some(st));
            return false;
        }
    };
    if keep {
        PENDING.with(|s| *s.borrow_mut() = Some(st));
    }
    true
}

/// A rejected click leaves the command armed (retry); the skylight and drains also stay
/// armed after success (placement until Esc), while the vault and attach finish.
fn click(ed: &mut Editor, st: &mut State, p: V2) -> bool {
    let prm = params();
    match &mut st.p {
        Pending::Barrel(pts) => {
            pts.push(p);
            if pts.len() < 3 {
                ed.say(Severity::Info, t("roof_edge.hint.barrel"));
                return true;
            }
            let (mut a, mut b, c) = (pts[0], pts[1], pts[2]);
            pts.clear();
            let d = b - a;
            let span = d.len();
            let mut depth = d.cross(c - a) / span.max(1e-12);
            // depth goes left of a→b; a point on the right flips the span
            if depth < 0.0 {
                std::mem::swap(&mut a, &mut b);
                depth = -depth;
            }
            let Some(stype) = slab_type(&ed.model, prm.slab_type) else {
                ed.say(Severity::Refusal, t("roof_edge.need_type"));
                return false;
            };
            let (level, rise, off) = (ed.level, span * prm.barrel_rise, prm.barrel_offset);
            let (a, b) = ((a.x, a.y), (b.x, b.y));
            !run(ed, "roof_edge.done", move |m| {
                m.add_barrel_roof(level, stype, off, a, b, rise, depth)
                    .map(|_| ())
            })
        }
        Pending::Join(roof) => {
            let roof = *roof;
            let Some(target) = roof_at(ed, p, Some(roof)) else {
                ed.say(Severity::Refusal, t("roof_edge.need_roof"));
                return true;
            };
            !run(ed, "roof_edge.done", move |m| m.join_roof(roof, target))
        }
        Pending::Skylight => {
            let Some(roof) = roof_at(ed, p, None) else {
                ed.say(Severity::Refusal, t("roof_edge.need_roof"));
                return true;
            };
            let (width, length, curb) = prm.sky;
            let k = SkylightSpec {
                roof,
                center: (p.x, p.y),
                width,
                length,
                curb,
            };
            run(ed, "roof_edge.hint.skylight_next", move |m| {
                m.add_skylight(&k).map(|_| ())
            });
            true
        }
        Pending::Drains(taper) => {
            let (taper, at) = (*taper, (p.x, p.y));
            run(ed, "roof_edge.hint.drains", move |m| {
                m.add_drain(taper, at).map(|_| ())
            });
            true
        }
    }
}

/// Midpoint of arc p→q with bulge `b`, and the bulge of each half (tan(θ/8) = tan(atan b / 2)).
pub fn split_arc(p: V2, q: V2, b: f64) -> (V2, f64) {
    let d = q - p;
    let c = d.len();
    let mid = (p + q) * 0.5;
    if c < 1e-12 || b == 0.0 {
        return (mid, 0.0);
    }
    // b > 0 means the arc is right of p→q; sagitta = b·c/2
    let right = v2(d.y, -d.x) * (1.0 / c);
    (mid + right * (b * c / 2.0), (b.atan() / 2.0).tan())
}

// ---------------------------------------------------------------- 3D

fn push(out: &mut Vec<SceneMesh>, tag: u32, m: &ddc_geom::mesh::Mesh, dz: f64) {
    if m.triangles.is_empty() {
        return;
    }
    out.push(SceneMesh {
        element: OTHER_LEVEL,
        tag,
        positions: m
            .positions
            .iter()
            .map(|p| [p.x as f32, p.y as f32, (p.z - dz) as f32])
            .collect(),
        triangles: m.triangles.clone(),
    });
}

/// Level 3D: profiles (without rustication), skylights, soffits, wedge, from the model cache (recomputed only
/// after the file is edited). Broken ones go to problems.
pub fn scene_meshes(ed: &Editor, out: &mut Vec<SceneMesh>, issues: &mut Vec<String>) {
    let m = &ed.model;
    // when the 3D registry is built, the `roof_edge_scene` provider draws the bodies
    if !ed.scene_providers {
        match m.roof_edge_bodies(Some(ed.level)) {
            Ok(bs) => {
                for (kind, _, mesh, _) in &bs {
                    push(out, tag_of(kind), mesh, ed.elevation);
                }
            }
            Err(e) => issues.push(format!("{}: {e}", t("roof_edge.failed"))),
        }
    }
    issues.extend(
        m.roof_edge_issues()
            .unwrap_or_default()
            .into_iter()
            .map(|i| format!("{} {}: {}", i.table, i.id, i.problem)),
    );
}

/// The level has edge bodies (a roof-only level is visible in the 3D "all levels" view).
pub fn level_has_bodies(m: &Model, level: i64) -> bool {
    m.roof_edge_bodies(Some(level)).is_ok_and(|b| !b.is_empty())
}

/// Rustication cutters of the current level for wall cutouts (elevations relative to the level).
pub fn reveal_cutters(ed: &Editor) -> Vec<ddc_geom::cut::Cutter> {
    ed.model
        .reveal_cutters(ed.level, ed.elevation)
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ddc_model::{LayerSpec, RoofKind, RoofSpec};
    use ddc_ui::i18n::{Catalog, Lang};

    #[test]
    fn commands_have_labels_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let mut keys: Vec<String> =
                Cmd::ALL.iter().map(|k| format!("cmd.{}", k.id())).collect();
            keys.extend(
                [
                    "cmd.arch.roof_gable",
                    "cmd.arch.roof_edges",
                    "cmd.arch.sweep_profiles",
                    "roof_edge.failed",
                    "roof_edge.deleted",
                    "roof_edge.hint.drains",
                    "roof_edge.hint.skylight_next",
                ]
                .map(String::from),
            );
            for id in keys {
                assert_ne!(c.t(&id), id, "{lang:?} {id}");
            }
        }
    }

    fn set(p: Pending, level: i64) {
        PENDING.with(|s| *s.borrow_mut() = Some(State { p, level }));
    }

    /// Editor with a covering type and a 10x6 hip roof (overhang 0.5) on the current level.
    pub(crate) fn with_roof() -> (Editor, i64, i64) {
        let mut ed = Editor::new_in_memory().unwrap();
        let st = ed
            .model
            .add_slab_type(
                "Roof",
                &[LayerSpec {
                    material: "Deck".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let roof = ed
            .model
            .add_roof(&RoofSpec {
                level: ed.level,
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
        ed.reload().unwrap();
        (ed, st, roof)
    }

    #[test]
    fn pending_click_is_consumed_and_escape_clears() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.set_mode(crate::wall_tool::Mode::Select);
        set(Pending::Skylight, ed.level);
        assert!(!handle(&mut ed, Event::Move(v2(0.0, 0.0))));
        assert!(handle(&mut ed, Event::Key(Key::Escape)));
        assert!(!handle(&mut ed, Event::Click(v2(0.0, 0.0))));
    }

    #[test]
    fn pending_dropped_on_level_tool_undo_and_missing_target() {
        let mut ed = Editor::new_in_memory().unwrap();
        let l = ed.level;
        set(Pending::Skylight, l + 1000);
        assert!(!handle(&mut ed, Event::Click(v2(0.0, 0.0))), "other level");
        assert!(!armed());
        set(Pending::Skylight, l);
        ed.set_mode(Mode::Polyline);
        assert!(
            !handle(&mut ed, Event::Click(v2(0.0, 0.0))),
            "wall tool active"
        );
        assert!(!armed());
        ed.set_mode(Mode::Select);
        set(Pending::Drains(999), l);
        assert!(!handle(&mut ed, Event::Click(v2(0.0, 0.0))), "taper gone");
        set(Pending::Join(999), l);
        assert!(!handle(&mut ed, Event::Click(v2(0.0, 0.0))), "roof gone");
        assert!(!armed());
        set(Pending::Skylight, l);
        ed.undo();
        assert!(!armed(), "undo resets");
        set(Pending::Skylight, l);
        ed.redo();
        assert!(!armed(), "redo resets");
    }

    #[test]
    fn skylight_stays_armed_after_success_and_failure_with_session_size() {
        let (mut ed, _, roof) = with_roof();
        set_params(Params {
            sky: (0.6, 0.9, 0.1),
            ..Params::default()
        });
        assert!(exec(&mut ed, Cmd::Skylight, &params()));
        assert!(
            handle(&mut ed, Event::Click(v2(100.0, 100.0))),
            "miss is consumed"
        );
        assert!(armed(), "miss keeps armed");
        assert!(handle(&mut ed, Event::Click(v2(5.0, 1.0))));
        assert!(armed(), "success keeps armed for the next one");
        let ks = ed.model.skylights(roof).unwrap();
        assert_eq!(ks.len(), 1);
        let k = ed.model.skylight(ks[0]).unwrap();
        assert_eq!((k.width, k.length, k.curb), (0.6, 0.9, 0.1));
        assert!(handle(&mut ed, Event::Key(Key::Escape)));
        assert!(!armed());
    }

    #[test]
    fn barrel_failure_keeps_armed_success_finishes() {
        let (mut ed, st, _) = with_roof();
        set_params(Params {
            slab_type: Some(st),
            barrel_offset: 2.5,
            ..Params::default()
        });
        assert!(exec(&mut ed, Cmd::Barrel, &params()));
        for q in [v2(20.0, 0.0), v2(26.0, 0.0), v2(30.0, 0.0)] {
            assert!(handle(&mut ed, Event::Click(q)));
        }
        assert!(armed(), "zero depth refused, still armed");
        for q in [v2(20.0, 0.0), v2(26.0, 0.0), v2(23.0, 8.0)] {
            assert!(handle(&mut ed, Event::Click(q)));
        }
        assert!(!armed(), "vault created");
        let id = level_roofs(&ed).into_iter().max().unwrap();
        assert_eq!(ed.model.roof(id).unwrap().offset, 2.5);
        let (bulges, ..) = ed.model.roof_ext(id).unwrap().unwrap();
        assert!(
            (bulges[0].abs() - 0.5).abs() < 1e-9,
            "rise = span/4 → |bulge| 0.5"
        );
    }

    #[test]
    fn soffit_and_fascia_use_params_and_render_cached_bodies() {
        let (mut ed, _, roof) = with_roof();
        let p = Params {
            soffit: (0.1, 0.05),
            ..Params::default()
        };
        assert!(exec(&mut ed, Cmd::Soffit, &p));
        let s = ed.model.soffits(roof).unwrap();
        assert_eq!(s.len(), 1);
        let (r, dh, th, _) = ed.model.soffit(s[0]).unwrap();
        assert_eq!((r, dh, th), (roof, 0.1, 0.05));
        assert!(exec(&mut ed, Cmd::Fascia, &p));
        let mut out = vec![];
        let mut issues = vec![];
        scene_meshes(&ed, &mut out, &mut issues);
        assert!(out.len() >= 2, "soffit + fascia bodies");
        assert!(out
            .iter()
            .all(|m| m.element == OTHER_LEVEL && m.tag == TAG_SLAB));
        assert!(level_has_bodies(&ed.model, ed.level));
    }

    #[test]
    fn delete_elems_is_one_undo_step() {
        let (mut ed, _, roof) = with_roof();
        let p = Params::default();
        assert!(exec(&mut ed, Cmd::Soffit, &p));
        assert!(exec(&mut ed, Cmd::Fascia, &p));
        let so = ed.model.soffits(roof).unwrap()[0];
        let sw = ed.model.sweeps(Some(ed.level)).unwrap()[0];
        assert!(delete_elems(
            &mut ed,
            &[Elem::Soffit(so), Elem::Sweep(sw), Elem::Soffit(so)]
        ));
        assert!(ed.model.soffits(roof).unwrap().is_empty());
        assert!(ed.model.sweeps(Some(ed.level)).unwrap().is_empty());
        ed.undo();
        assert_eq!(ed.model.soffits(roof).unwrap(), vec![so]);
        assert_eq!(ed.model.sweeps(Some(ed.level)).unwrap(), vec![sw]);
        assert!(!delete_elems(&mut ed, &[Elem::Taper(999)]));
        assert_eq!(
            ed.model.soffits(roof).unwrap(),
            vec![so],
            "failed delete changes nothing"
        );
    }

    #[test]
    fn roof_only_level_is_in_all_levels_scene() {
        let (mut ed, _, roof) = with_roof();
        assert!(exec(&mut ed, Cmd::Soffit, &Params::default()));
        let l2 = ed.model.add_level("L2", 6.0).unwrap();
        ed.reload().unwrap();
        ed.set_level(l2);
        assert_eq!(ed.level, l2);
        let n = ed.scene_all().meshes.len();
        let soffit = ed.model.soffits(roof).unwrap()[0];
        assert!(delete_elems(&mut ed, &[Elem::Soffit(soffit)]));
        assert!(
            ed.scene_all().meshes.len() < n,
            "soffit of the roof-only level was drawn"
        );
    }

    #[test]
    fn elem_round_trips_and_arc_split() {
        for e in [
            Elem::Sweep(3),
            Elem::Skylight(4),
            Elem::Soffit(5),
            Elem::Taper(6),
        ] {
            assert_eq!(Elem::from_index(e.index(), e.id()), Some(e));
            assert_eq!(Elem::from_kind(e.kind(), e.id()), Some(e));
        }
        assert_eq!(
            Elem::from_kind("skylight_glass", 4),
            Some(Elem::Skylight(4))
        );
        let (m, h) = split_arc(v2(0.0, 0.0), v2(2.0, 0.0), 1.0);
        assert!((m - v2(1.0, -1.0)).len() < 1e-12);
        assert!((h - (std::f64::consts::PI / 8.0).tan()).abs() < 1e-12);
        assert_eq!(
            split_arc(v2(0.0, 0.0), v2(2.0, 0.0), 0.0),
            (v2(1.0, 0.0), 0.0)
        );
    }
}
