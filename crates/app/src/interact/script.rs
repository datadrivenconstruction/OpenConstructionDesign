// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Event scripts (INTERACTIONS.md §2): `parse` text into steps, `run` them on an
//! `Editor` without a window and compare the checks, `record` a live session back into text.
//! Deterministic: time comes only from `tick`/`hover`, no clock, no randomness.
use super::input::{Btn, Input, Key, Mods};
use super::adapter::{PlanAdapter, SectionAdapter, View3dAdapter, ViewAdapter};
use super::pointer::{Pointer, ViewKind, WorkPlane};
use super::scope::{Scope, ToolId};
use super::Interaction;
use crate::editor::Editor;
use crate::wall_tool::Mode;
use ddc_geom::{v2, V2};
use std::fmt;

/// Snap radius in screen px of a mouse (INTERACTIONS §0.6): `zoom=` turns it into metres.
const CAPTURE_PX: f64 = 12.0;
/// Moves between press and release of `drag`.
const DRAG_MOVES: usize = 8;
const TOL: f64 = 1e-9;

#[derive(Clone, Debug, PartialEq)]
pub struct ParseErr {
    pub line: usize,
    pub col: usize,
    pub msg: String,
}

impl fmt::Display for ParseErr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.msg)
    }
}

/// Model state before the first event (lines of `key=value` before any event).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Setup {
    /// px per metre: the snap radius becomes `12 px / zoom`; `None` keeps the tool's radius.
    pub zoom: Option<f64>,
    /// Walls of the current type, drawn before the run (one record).
    pub walls: Vec<(V2, V2)>,
    /// Elevations of extra levels above the first one.
    pub levels: Vec<f64>,
    /// Active level, 1-based by elevation (`level=2`: the first extra level); walls go on it.
    pub level: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Ev {
    In(Input),
    /// Advance the script clock by ms and send `Tick`.
    Wait(u64),
    /// Hotkey (`WA`) or ribbon command id (`arch.wall`).
    Tool(String),
    Undo,
    Redo,
    /// Delete the selection.
    Delete,
    /// `mod(Alt, down)`: modifier state; Alt also turns snapping off.
    Mod(ModKey, bool),
    /// `view(plan|3d|section:N)`: the following pointer events come through that view.
    View(ViewKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModKey {
    Shift,
    Ctrl,
    Alt,
}

#[derive(Clone, Debug, PartialEq)]
pub enum What {
    Walls(usize),
    /// Points where two or more wall ends meet.
    Joints(usize),
    /// Undo records made since the run started.
    Journal(u64),
    Status(String),
    StatusPrefix(String),
    /// `Idle`, or a drafting scope with `n` collected points.
    State(Option<usize>),
    /// Scope kind by name (`GripDrag`).
    Scope(String),
    Selected(usize),
    /// Walls the last event added.
    Commit(usize),
    /// `<tool>.<step>` of the scope.
    Hint(String),
    /// Refusal code or message key of the last event.
    Refusal(String),
    /// Kind of the current snap (`Endpoint`).
    Snap(String),
    /// Point of the current snap.
    SnapAt(V2),
    /// Row count of a model table.
    Rows(String, usize),
    /// End `a` (false) or `b` (true) of the wall at this index of the editor's list.
    WallEnd(usize, bool, V2),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub line: usize,
    pub what: What,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Script {
    pub setup: Setup,
    pub steps: Vec<(Ev, Vec<Check>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub line: usize,
    pub want: String,
    pub got: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub failures: Vec<Failure>,
    /// Scope after every step, in order.
    pub scopes: Vec<Scope>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }
}

// ---------- parsing ----------

#[derive(Debug)]
enum Tok {
    /// `name(args)` or a bare word (`undo`).
    Call(String, Option<String>),
    Arrow,
    /// `name=value` or `name^=value`.
    Check(String, bool, String),
}

struct Lexer<'a> {
    s: &'a [char],
    i: usize,
    line: usize,
    col0: usize,
}

impl Lexer<'_> {
    fn err(&self, at: usize, msg: impl Into<String>) -> ParseErr {
        ParseErr { line: self.line, col: at - self.col0 + 1, msg: msg.into() }
    }

    /// Text up to the bracket that closes the one at `i` (quotes respected).
    fn group(&mut self, open: char, close: char) -> Result<String, ParseErr> {
        let start = self.i;
        let mut depth = 0;
        let mut quoted = false;
        while self.i < self.s.len() {
            let c = self.s[self.i];
            self.i += 1;
            match c {
                '"' => quoted = !quoted,
                _ if quoted => {}
                c if c == open => depth += 1,
                c if c == close => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(self.s[start + 1..self.i - 1].iter().collect());
                    }
                }
                _ => {}
            }
        }
        Err(self.err(start, format!("unclosed '{open}'")))
    }

    fn value(&mut self) -> Result<String, ParseErr> {
        let start = self.i;
        let mut out = String::new();
        while self.i < self.s.len() && !self.s[self.i].is_whitespace() {
            match self.s[self.i] {
                '"' => {
                    self.i += 1;
                    let q = self.i;
                    while self.i < self.s.len() && self.s[self.i] != '"' {
                        self.i += 1;
                    }
                    if self.i >= self.s.len() {
                        return Err(self.err(start, "unclosed '\"'"));
                    }
                    out.push('"');
                    out.extend(&self.s[q..self.i]);
                    out.push('"');
                    self.i += 1;
                }
                '(' | '[' | '{' => {
                    let (o, c) = match self.s[self.i] {
                        '(' => ('(', ')'),
                        '[' => ('[', ']'),
                        _ => ('{', '}'),
                    };
                    let g = self.group(o, c)?;
                    out.push(o);
                    out.push_str(&g);
                    out.push(c);
                }
                ';' => break,
                ch => {
                    out.push(ch);
                    self.i += 1;
                }
            }
        }
        Ok(out)
    }

    fn next(&mut self) -> Result<Option<(usize, Tok)>, ParseErr> {
        while self.i < self.s.len() && (self.s[self.i].is_whitespace() || self.s[self.i] == ';') {
            self.i += 1;
        }
        if self.i >= self.s.len() || self.s[self.i] == '#' {
            return Ok(None);
        }
        let at = self.i;
        let c = self.s[at];
        if c == '→' {
            self.i += 1;
            return Ok(Some((at, Tok::Arrow)));
        }
        if c == '-' && self.s.get(at + 1) == Some(&'>') {
            self.i += 2;
            return Ok(Some((at, Tok::Arrow)));
        }
        let word_ch = |c: char| c.is_alphanumeric() || c == '_' || c == '.';
        while self.i < self.s.len() && word_ch(self.s[self.i]) {
            self.i += 1;
        }
        // an index inside a check name: `wall[0].b`
        while self.i > at && self.s.get(self.i) == Some(&'[') {
            self.group('[', ']')?;
            while self.i < self.s.len() && word_ch(self.s[self.i]) {
                self.i += 1;
            }
        }
        let name: String = self.s[at..self.i].iter().collect();
        if name.is_empty() {
            return Err(self.err(at, format!("unexpected '{c}'")));
        }
        match self.s.get(self.i) {
            Some('(') => {
                let args = self.group('(', ')')?;
                // `rows(table)=N` is a check with an argument
                if self.s.get(self.i) == Some(&'=') {
                    self.i += 1;
                    let v = self.value()?;
                    return Ok(Some((at, Tok::Check(format!("{name}({args})"), false, v))));
                }
                Ok(Some((at, Tok::Call(name, Some(args)))))
            }
            Some('=') => {
                self.i += 1;
                Ok(Some((at, Tok::Check(name, false, self.value()?))))
            }
            Some('^') if self.s.get(self.i + 1) == Some(&'=') => {
                self.i += 2;
                Ok(Some((at, Tok::Check(name, true, self.value()?))))
            }
            _ => Ok(Some((at, Tok::Call(name, None)))),
        }
    }
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// `x,y` → point.
fn point(s: &str) -> Option<V2> {
    let (x, y) = s.split_once(',')?;
    Some(v2(num(x)?, num(y)?))
}

fn unquote(s: &str) -> Option<String> {
    let t = s.trim();
    (t.len() >= 2 && t.starts_with('"') && t.ends_with('"')).then(|| t[1..t.len() - 1].to_string())
}

fn key_named(s: &str) -> Option<Ev> {
    Some(Ev::In(match s.trim() {
        "Enter" => Input::Key(Key::Enter, Mods::NONE),
        "Escape" | "Esc" => Input::Key(Key::Escape, Mods::NONE),
        "Backspace" => Input::Key(Key::Backspace, Mods::NONE),
        "Tab" => Input::Key(Key::Tab, Mods::NONE),
        "Close" => Input::Key(Key::Close, Mods::NONE),
        "Arc" => Input::Key(Key::Arc, Mods::NONE),
        "Space" => Input::Char(' '),
        "Delete" => return Some(Ev::Delete),
        _ => return None,
    }))
}

fn events(name: &str, args: Option<&str>) -> Result<Vec<Ev>, String> {
    let need = || args.ok_or_else(|| format!("{name}: arguments expected"));
    let pt = |s: &str| point(s).ok_or_else(|| format!("{name}: expected (x,y), got ({s})"));
    let ptr = |s: &str| pt(s).map(Pointer::from);
    let one = |e: Input| Ok(vec![Ev::In(e)]);
    match name {
        "move" => one(Input::Move(ptr(need()?)?)),
        "click" => one(Input::Click(ptr(need()?)?, Btn::Left)),
        "rmb" => one(Input::Click(ptr(need()?)?, Btn::Right)),
        "dblclick" => one(Input::DblClick(ptr(need()?)?)),
        "press" => one(Input::Press(ptr(need()?)?, Btn::Left)),
        "release" => one(Input::Release(ptr(need()?)?, Btn::Left)),
        "drag" => {
            let a = need()?;
            let (p, q) = a
                .split_once('→')
                .or_else(|| a.split_once("->"))
                .ok_or_else(|| "drag: expected (x1,y1→x2,y2)".to_string())?;
            let (p, q) = (pt(p)?, pt(q)?);
            let mut v = vec![Ev::In(Input::Press(p.into(), Btn::Left))];
            for i in 1..=DRAG_MOVES {
                let t = i as f64 / DRAG_MOVES as f64;
                v.push(Ev::In(Input::Move((p + (q - p) * t).into())));
            }
            v.push(Ev::In(Input::Release(q.into(), Btn::Left)));
            Ok(v)
        }
        "key" => key_named(need()?).map(|e| vec![e]).ok_or_else(|| format!("key: unknown key {}", need().unwrap_or(""))),
        "type" => {
            let s = unquote(need()?).ok_or_else(|| "type: expected \"text\"".to_string())?;
            Ok(s.chars().map(|c| Ev::In(Input::Char(c))).collect())
        }
        "mod" => {
            let a = need()?;
            let (k, d) = a.split_once(',').ok_or_else(|| "mod: expected (Shift|Ctrl|Alt, down|up)".to_string())?;
            let k = match k.trim() {
                "Shift" => ModKey::Shift,
                "Ctrl" => ModKey::Ctrl,
                "Alt" => ModKey::Alt,
                o => return Err(format!("mod: unknown modifier {o}")),
            };
            let down = match d.trim() {
                "down" => true,
                "up" => false,
                o => return Err(format!("mod: expected down or up, got {o}")),
            };
            Ok(vec![Ev::Mod(k, down)])
        }
        "hover" => {
            let a = need()?;
            let (xy, ms) = a.rsplit_once(',').ok_or_else(|| "hover: expected (x,y,ms)".to_string())?;
            let ms = ms.trim().parse::<u64>().map_err(|_| format!("hover: bad ms {ms}"))?;
            Ok(vec![Ev::In(Input::Move(ptr(xy)?)), Ev::Wait(ms)])
        }
        "tick" => {
            let a = need()?;
            Ok(vec![Ev::Wait(a.trim().parse::<u64>().map_err(|_| format!("tick: bad ms {a}"))?)])
        }
        "tool" => {
            let t = need()?.trim().to_string();
            if arm_fn(&t).is_none() {
                return Err(format!("tool: unknown tool {t}"));
            }
            Ok(vec![Ev::Tool(t)])
        }
        "view" => {
            let v = need()?.trim();
            let kind = match v {
                "plan" => ViewKind::Plan { level: 0 },
                "3d" => ViewKind::View3d,
                _ => match v.strip_prefix("section:").and_then(|n| n.trim().parse().ok()) {
                    Some(view) => ViewKind::Section { view },
                    None => return Err(format!("view: view not supported: {v}")),
                },
            };
            Ok(vec![Ev::View(kind)])
        }
        "undo" if args.is_none() => Ok(vec![Ev::Undo]),
        "redo" if args.is_none() => Ok(vec![Ev::Redo]),
        _ => Err(format!("unknown event {name}")),
    }
}

fn check(name: &str, prefix: bool, v: &str) -> Result<What, String> {
    let count = || v.parse::<usize>().map_err(|_| format!("{name}: expected a count, got {v}"));
    let text = || unquote(v).ok_or_else(|| format!("{name}: expected \"text\""));
    if prefix {
        return match name {
            "status" => Ok(What::StatusPrefix(text()?)),
            _ => Err(format!("{name}: no prefix form")),
        };
    }
    if let Some(t) = name.strip_prefix("rows(").and_then(|t| t.strip_suffix(')')) {
        let ok = !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        return if ok { Ok(What::Rows(t.to_string(), count()?)) } else { Err(format!("rows: bad table {t}")) };
    }
    if let Some(r) = name.strip_prefix("wall[") {
        let (i, end) = r.split_once(']').ok_or_else(|| format!("{name}: expected wall[i].a or wall[i].b"))?;
        let i = i.parse::<usize>().map_err(|_| format!("{name}: bad index {i}"))?;
        let b = match end {
            ".a" => false,
            ".b" => true,
            _ => return Err(format!("{name}: expected wall[i].a or wall[i].b")),
        };
        let p = v.strip_prefix('(').and_then(|r| r.strip_suffix(')')).and_then(point);
        return p.map(|p| What::WallEnd(i, b, p)).ok_or_else(|| format!("{name}: expected (x,y), got {v}"));
    }
    match name {
        "walls" => Ok(What::Walls(count()?)),
        "joints" => Ok(What::Joints(count()?)),
        "selected" => Ok(What::Selected(count()?)),
        "journal" => v
            .strip_prefix('+')
            .and_then(|n| n.parse().ok())
            .map(What::Journal)
            .ok_or_else(|| format!("journal: expected +N, got {v}")),
        "status" => Ok(What::Status(text()?)),
        "hint" => Ok(What::Hint(text()?)),
        "state" => {
            if v == "Idle" {
                return Ok(What::State(None));
            }
            let n = v
                .strip_prefix("Drafting{")
                .or_else(|| v.strip_prefix("Drawing{"))
                .and_then(|r| r.strip_suffix('}'))
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| format!("state: expected Idle or Drafting{{n}}, got {v}"))?;
            Ok(What::State(Some(n)))
        }
        "scope" => {
            let ok = super::ScopeKind::ALL.iter().any(|k| format!("{k:?}") == v);
            if ok { Ok(What::Scope(v.to_string())) } else { Err(format!("scope: unknown kind {v}")) }
        }
        "outcome" => v
            .strip_prefix("Commit(")
            .and_then(|r| r.strip_suffix(')'))
            .and_then(|n| n.parse().ok())
            .map(What::Commit)
            .ok_or_else(|| format!("outcome: expected Commit(n), got {v}")),
        "refusal" => Ok(What::Refusal(v.to_string())),
        "snap" => Ok(What::Snap(v.to_string())),
        "p" => v
            .strip_prefix('(')
            .and_then(|r| r.strip_suffix(')'))
            .and_then(point)
            .map(What::SnapAt)
            .ok_or_else(|| format!("p: expected (x,y), got {v}")),
        _ => Err(format!("unknown check {name}")),
    }
}

fn setup_line(s: &mut Setup, name: &str, v: &str) -> Result<(), String> {
    match name {
        "zoom" => {
            s.zoom = Some(num(v).filter(|z| *z > 0.0).ok_or_else(|| format!("zoom: bad value {v}"))?);
        }
        "walls" => {
            let body = v.strip_prefix('[').and_then(|r| r.strip_suffix(']')).ok_or("walls: expected [(x,y)-(x,y) …]")?;
            for seg in body.split_whitespace() {
                let (a, b) = seg.split_once(")-(").ok_or_else(|| format!("walls: bad segment {seg}"))?;
                let a = a.strip_prefix('(').and_then(point);
                let b = b.strip_suffix(')').and_then(point);
                match (a, b) {
                    (Some(a), Some(b)) => s.walls.push((a, b)),
                    _ => return Err(format!("walls: bad segment {seg}")),
                }
            }
        }
        "levels" => {
            let body = v.strip_prefix('[').and_then(|r| r.strip_suffix(']')).ok_or("levels: expected [z …]")?;
            for z in body.split(',') {
                s.levels.push(num(z).ok_or_else(|| format!("levels: bad elevation {z}"))?);
            }
        }
        "level" => {
            let k: usize = v.trim().parse().map_err(|_| format!("level: expected 1, 2, …, got {v}"))?;
            s.level = Some(k);
        }
        _ => return Err(format!("unknown setting {name}")),
    }
    Ok(())
}

/// Parse a script. Lines marked `#pending` are skipped (behaviour of a later card).
pub fn parse(src: &str) -> Result<Script, ParseErr> {
    let mut sc = Script::default();
    for (n, raw) in src.lines().enumerate() {
        if raw.contains("#pending") {
            continue;
        }
        let chars: Vec<char> = raw.chars().collect();
        let mut lx = Lexer { s: &chars, i: 0, line: n + 1, col0: 0 };
        let mut checks = false;
        while let Some((at, t)) = lx.next()? {
            let err = |m: String| lx.err(at, m);
            match t {
                Tok::Arrow => {
                    if sc.steps.is_empty() {
                        return Err(err("'→' needs an event before it".into()));
                    }
                    checks = true;
                }
                Tok::Check(name, prefix, v) if checks => {
                    let what = check(&name, prefix, &v).map_err(err)?;
                    sc.steps.last_mut().expect("arrow follows an event").1.push(Check { line: n + 1, what });
                }
                Tok::Check(name, _, v) if sc.steps.is_empty() => {
                    setup_line(&mut sc.setup, &name, &v).map_err(err)?;
                }
                Tok::Check(name, ..) => return Err(err(format!("check {name} without '→'"))),
                Tok::Call(name, args) => {
                    checks = false;
                    let evs = events(&name, args.as_deref()).map_err(err)?;
                    sc.steps.extend(evs.into_iter().map(|e| (e, vec![])));
                }
            }
        }
    }
    Ok(sc)
}

// ---------- running ----------

type ArmFn = Box<dyn Fn(&mut Editor)>;

const MOD_OPS: [crate::modify_tools::Op; 12] = {
    use crate::modify_tools::Op::*;
    [Move, Copy, Rotate, Mirror, MirrorDraw, Array, Offset, Trim, Extend, Split, Align, Scale]
};

/// Arm a tool by hotkey (INTERACTIONS §0.1) or ribbon command id.
/// Arm a tool by hotkey or command id without the app shell (scripts, 3D creation on the
/// level plane); `false` for an unknown tool.
pub fn arm(ed: &mut Editor, name: &str) -> bool {
    match arm_fn(name) {
        Some(f) => {
            f(ed);
            Interaction::refresh(ed);
            true
        }
        None => false,
    }
}

fn arm_fn(name: &str) -> Option<ArmFn> {
    use crate::opening_tool::OpeningType;
    let lang = ddc_ui::i18n::Lang::En;
    let id = match name {
        "WA" => "arch.wall",
        "DR" => "arch.door",
        "WN" => "arch.window",
        "SB" => "arch.floor",
        "RM" => "arch.room",
        "CL" => "arch.column",
        "BM" => "str.beam",
        "TX" => "ann.text",
        "MV" => "mod.move",
        "CO" => "mod.copy",
        "RO" => "mod.rotate",
        "MM" => "mod.mirror",
        "AR" => "mod.array",
        "SC" => "mod.scale",
        "OF" => "mod.offset",
        "TR" => "mod.trim",
        "EX" => "mod.extend",
        "SP" => "mod.split",
        "AL" => "mod.align",
        o => o,
    };
    // the modify tools: one session for the plan, 3D and scripts
    if let Some(op) = MOD_OPS.iter().copied().find(|op| op.command() == id) {
        return Some(Box::new(move |ed: &mut Editor| {
            crate::interact::sessions::modify::start(ed, op);
        }));
    }
    let f: ArmFn = match id {
        "arch.wall" => Box::new(|ed: &mut Editor| ed.set_mode(Mode::Polyline)),
        "arch.wall_room" => Box::new(|ed: &mut Editor| ed.set_mode(Mode::Rect)),
        "arch.curtain_wall" => Box::new(|ed: &mut Editor| {
            ed.set_mode(Mode::Polyline);
            ed.tool.curtain = true;
        }),
        "arch.door" => Box::new(|ed: &mut Editor| ed.start_opening(OpeningType::door())),
        "arch.window" => Box::new(|ed: &mut Editor| ed.start_opening(OpeningType::window())),
        "arch.section" => Box::new(|ed: &mut Editor| ed.start_section()),
        "select" => Box::new(|ed: &mut Editor| ed.set_mode(Mode::Select)),
        // the selected floor's context commands: shape edit and boundary sketch
        "ctx.floor.shape" => Box::new(|ed: &mut Editor| {
            if let Some(id) = selected_slab(ed) {
                if let Err(e) = crate::shape_edit::begin(ed, id) {
                    ed.status = Some(e);
                }
            }
        }),
        "ctx.floor.sketch" => Box::new(|ed: &mut Editor| {
            if let Some(id) = selected_slab(ed) {
                crate::sketch_mode::begin(ed, ddc_model::SketchHost::Slab(id));
            }
        }),
        _ => {
            use crate::annot_text::Kind as A;
            let ann = [
                ("ann.text", A::Text),
                ("ann.leader", A::Leader),
                ("ann.detail_line", A::DetailLine),
                ("ann.region", A::Region),
                ("ann.spot_elev", A::SpotElev),
                ("ann.spot_coord", A::SpotCoord),
                ("ann.cloud", A::Cloud),
                ("ann.dim_chain", A::DimChain),
            ];
            use crate::topo::Tool as T;
            let topo = [T::Points, T::Boundary, T::Region, T::Pad, T::Smooth, T::Flatten, T::Edit];
            if let Some(&(_, k)) = ann.iter().find(|a| a.0 == id) {
                Box::new(move |ed: &mut Editor| crate::annot_text::arm(ed, k, lang))
            } else if let Some(&t) = topo.iter().find(|t| t.command() == id) {
                Box::new(move |ed: &mut Editor| crate::topo::arm(ed, t, lang))
            } else if let Some((k, v)) = crate::solid_tools::Kind::ALL
                .iter()
                .flat_map(|&k| [(k, false), (k, true)])
                .find(|&(k, v)| k.command(v) == id)
            {
                Box::new(move |ed: &mut Editor| crate::solid_tools::arm(ed, k, v, lang))
            } else {
                let k = crate::tools::Kind::ALL.iter().copied().find(|k| k.commands().iter().any(|c| *c == id))?;
                Box::new(move |ed: &mut Editor| crate::tools::arm(ed, k, lang))
            }
        }
    };
    Some(f)
}

fn selected_slab(ed: &Editor) -> Option<i64> {
    ed.selection.iter().find_map(|s| match s {
        crate::editor::Sel::Slab(id) => Some(*id),
        _ => None,
    })
}

// Snap state seam: the only reads and writes of snap state here, one place to follow the
// snap module's single source once it exists.

/// Kind and point of the snap the cursor holds now.
fn snap_now(ed: &Editor) -> Option<(crate::snap::SnapKind, V2)> {
    ed.tool.snaps.get(ed.tool.snap_idx).map(|s| (s.kind, s.p))
}

/// Alt held: points go exactly under the cursor.
fn set_free(ed: &mut Editor, on: bool) {
    ed.tool.no_snap = on;
    // the window re-snaps a resting cursor every frame; a script has no frames
    if !ed.tool.snaps.is_empty() {
        let c = ed.tool.cursor;
        ed.tool.track_in(ed.ix.draft_mut(), c);
    }
}

/// Points where two or more wall ends meet (`1e-6` m).
fn joints(ed: &Editor) -> usize {
    let mut ends: Vec<V2> = ed.walls.iter().flat_map(|w| [w.a, w.b]).collect();
    let mut n = 0;
    while let Some(p) = ends.pop() {
        let before = ends.len();
        ends.retain(|q| (*q - p).len() > 1e-6);
        if ends.len() < before {
            n += 1;
        }
    }
    n
}

fn points(ed: &Editor) -> usize {
    ed.model_tool().map_or(ed.wall_draft().points.len(), |a| a.pts.len())
}

fn rows(ed: &Editor, t: &str) -> Result<usize, String> {
    ed.model
        .sql()
        .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get::<_, i64>(0))
        .map(|n| n as usize)
        .map_err(|e| e.to_string())
}

fn hint(s: &Scope) -> String {
    match s {
        Scope::Drafting { tool, step } => format!("{}.{step}", tool.0),
        Scope::Placing { tool } => format!("{}.place", tool.0),
        o => format!("{:?}", o.kind()),
    }
}

/// Compare one check; `Err((want, got))` on a mismatch.
fn verify(ed: &Editor, base: u64, added: usize, w: &What) -> Result<(), (String, String)> {
    let eq = |want: String, got: String| if want == got { Ok(()) } else { Err((want, got)) };
    let scope = ed.ix.scope();
    let snap = snap_now(ed);
    match w {
        What::Walls(n) => eq(format!("walls={n}"), format!("walls={}", ed.walls.len())),
        What::Joints(n) => eq(format!("joints={n}"), format!("joints={}", joints(ed))),
        What::Journal(n) => eq(format!("journal=+{n}"), format!("journal=+{}", ed.pushes - base)),
        What::Selected(n) => eq(format!("selected={n}"), format!("selected={}", ed.selection.len())),
        What::Commit(n) => eq(format!("outcome=Commit({n})"), format!("outcome=Commit({added})")),
        What::Status(t) => eq(t.clone(), ed.status.clone().unwrap_or_default()),
        What::StatusPrefix(t) => {
            let got = ed.status.clone().unwrap_or_default();
            if got.starts_with(t.as_str()) { Ok(()) } else { Err((format!("{t}…"), got)) }
        }
        What::State(n) => {
            let got = match scope {
                Scope::Idle => "Idle".to_string(),
                Scope::Drafting { .. } => format!("Drafting{{{}}}", points(ed)),
                o => format!("{:?}", o.kind()),
            };
            eq(n.map_or("Idle".into(), |n| format!("Drafting{{{n}}}")), got)
        }
        What::Scope(k) => eq(k.clone(), format!("{:?}", scope.kind())),
        What::Hint(h) => eq(h.clone(), hint(scope)),
        What::Refusal(k) => {
            let by_value = ed.ix.last_refusal.as_ref().is_some_and(|r| r.code.as_str() == k.as_str() || r.msg_key == k.as_str());
            let refused = matches!(
                ed.status_sev.as_ref().map(|s| s.0),
                Some(crate::status::Severity::Refusal | crate::status::Severity::Error)
            );
            let shown = ed.status.clone().unwrap_or_default();
            if by_value || (refused && shown == crate::tr(k, &[])) {
                Ok(())
            } else {
                Err((format!("refusal={k}"), format!("status={shown:?}")))
            }
        }
        What::Snap(k) => {
            let got = snap.map_or("none".to_string(), |s| format!("{:?}", s.0));
            let base_kind = got.split('(').next().unwrap_or("");
            if base_kind == k.as_str() { Ok(()) } else { Err((format!("snap={k}"), format!("snap={got}"))) }
        }
        What::SnapAt(p) => {
            let got = snap.map(|s| s.1);
            match got {
                Some(q) if (q - *p).len() <= TOL => Ok(()),
                _ => Err((format!("p=({},{})", p.x, p.y), format!("{got:?}"))),
            }
        }
        What::Rows(t, n) => match rows(ed, t) {
            Ok(m) => eq(format!("rows({t})={n}"), format!("rows({t})={m}")),
            Err(e) => Err((format!("rows({t})={n}"), e)),
        },
        What::WallEnd(i, b, p) => {
            let want = format!("wall[{i}].{}=({},{})", if *b { 'b' } else { 'a' }, p.x, p.y);
            match ed.walls.get(*i).map(|w| if *b { w.b } else { w.a }) {
                Some(q) if (q - *p).len() <= TOL => Ok(()),
                got => Err((want, format!("{got:?}"))),
            }
        }
    }
}

/// Play `s` on `ed` and collect the checks that do not hold.
/// Screen of the script views, px.
const SCREEN: [f64; 2] = [1600.0, 1200.0];

/// The adapter of `v` for the editor's active level. The plan shows 100 px/m around the
/// origin; the 3D view is the default orbit (azimuth and tilt of a new view) 40 m from the
/// origin; a section is the editor's view `N`. The level plane is the work plane.
pub fn adapter_of(ed: &Editor, v: ViewKind) -> Option<Box<dyn ViewAdapter>> {
    let c = [SCREEN[0] / 2.0, SCREEN[1] / 2.0];
    Some(match v {
        ViewKind::Plan { .. } => Box::new(PlanAdapter {
            center: v2(0.0, 0.0),
            scale: 100.0,
            canvas: c,
            level: ed.level,
            z: ed.elevation,
        }),
        ViewKind::View3d => {
            let cam = ddc_render::OrbitCamera { distance: 40.0, ..Default::default() };
            let plane = WorkPlane::level(ed.level, ed.elevation);
            Box::new(View3dAdapter::new(&cam, [0.0, 0.0], SCREEN, ed.elevation, plane))
        }
        ViewKind::Section { view } => {
            let sp = ed.view_spec(view)?;
            let (a, b) = crate::editor::sec::ends(sp);
            Box::new(SectionAdapter::new(view, a, b, v2(0.0, 0.0), 100.0, c)?)
        }
    })
}

/// The script's plan point seen through `ad`: model point on the level → screen → pointer.
fn through(ed: &Editor, ad: &dyn ViewAdapter, i: Input) -> Option<Input> {
    i.map_pointer(|p| {
        let q = p.plan()?;
        if ed.opening_mode {
            if let Some(h) = super::host::aim(ed, ad, q) {
                return Some(h);
            }
        }
        let s = ad.project_at(ddc_geom::v3(q.x, q.y, ed.elevation))?;
        Some(ad.pointer_at(s))
    })
}

pub fn run(ed: &mut Editor, s: &Script) -> Report {
    run_in(ed, s, None)
}

/// `run` with every pointer event coming through `view` until the script names another one
/// (the parity gate replays one script through each view).
pub fn run_in(ed: &mut Editor, s: &Script, view: Option<ViewKind>) -> Report {
    if let Some(z) = s.setup.zoom {
        ed.tool.radius = CAPTURE_PX / z;
    }
    let mut report = Report::default();
    for (i, z) in s.setup.levels.iter().enumerate() {
        if let Err(e) = ed.model.add_level(&format!("L{}", i + 2), *z) {
            report.failures.push(Failure { line: 0, want: format!("level at {z}"), got: e.to_string() });
        }
    }
    if let Some(k) = s.setup.level {
        let ls = ed.model.level_list().unwrap_or_default();
        match ls.get(k.wrapping_sub(1)) {
            Some(l) => ed.set_level(l.0),
            None => report.failures.push(Failure { line: 0, want: format!("level {k}"), got: format!("{} levels", ls.len()) }),
        }
    }
    if !s.setup.walls.is_empty() {
        ed.commit_walls(s.setup.walls.clone());
    }
    let base = ed.pushes;
    let mut now = 0u64;
    let mut mods = Mods::NONE;
    let mut ad = view.and_then(|v| adapter_of(ed, v));
    for (ev, checks) in &s.steps {
        let walls = ed.walls.len();
        match ev {
            Ev::In(i) => match &ad {
                Some(a) => {
                    if let Some(i) = through(ed, a.as_ref(), *i) {
                        Interaction::update(ed, i);
                    }
                }
                None => Interaction::update(ed, *i),
            },
            Ev::View(v) => {
                // a forced view (parity gate) overrides the script's own
                if view.is_none() {
                    ad = adapter_of(ed, *v);
                    if ad.is_none() {
                        report.failures.push(Failure { line: 0, want: format!("view {v:?}"), got: "no such view".into() });
                    }
                }
            }
            Ev::Wait(ms) => {
                now += ms;
                Interaction::update(ed, Input::Tick(now));
            }
            Ev::Tool(t) => {
                if let Some(f) = arm_fn(t) {
                    f(ed);
                    Interaction::refresh(ed);
                }
            }
            Ev::Undo => ed.undo(),
            Ev::Redo => ed.redo(),
            Ev::Delete => {
                if !ed.tool_armed() {
                    ed.delete_selection();
                }
            }
            Ev::Mod(k, down) => {
                match k {
                    ModKey::Shift => mods.shift = *down,
                    ModKey::Ctrl => mods.ctrl = *down,
                    ModKey::Alt => {
                        mods.alt = *down;
                        set_free(ed, *down);
                    }
                }
                Interaction::update(ed, Input::Mods(mods));
            }
        }
        report.scopes.push(*ed.ix.scope());
        let added = ed.walls.len().saturating_sub(walls);
        for c in checks {
            if let Err((want, got)) = verify(ed, base, added, &c.what) {
                report.failures.push(Failure { line: c.line, want, got });
            }
        }
    }
    report
}

// ---------- recording ----------

/// Start recording the inputs of the live session; the current tool opens the script.
pub fn record_start(ed: &mut Editor) {
    ed.ix.rec = Some((ed.ix.scope().tool(), vec![]));
}

fn xy(p: Pointer) -> String {
    let q = p.plan().unwrap_or_default();
    format!("{},{}", q.x, q.y)
}

/// The recorded session as a script that replays to the same journal.
pub fn record(ed: &Editor) -> String {
    let Some((tool, inputs)) = &ed.ix.rec else {
        return String::new();
    };
    let mut out: Vec<String> = tool.map(|ToolId(t)| format!("tool({t})")).into_iter().collect();
    let mut typed = String::new();
    let mut last_tick = 0u64;
    let mut mods = Mods::NONE;
    let flush = |typed: &mut String, out: &mut Vec<String>| {
        if !typed.is_empty() {
            out.push(format!("type(\"{typed}\")"));
            typed.clear();
        }
    };
    for i in inputs {
        if let Input::Char(c) = i {
            if *c == ' ' {
                flush(&mut typed, &mut out);
                out.push("key(Space)".into());
            } else if *c != '"' {
                typed.push(*c);
            }
            continue;
        }
        flush(&mut typed, &mut out);
        // a script speaks plan points: a pointer that gives none is not recorded
        if i.pointer().is_some() && i.point().is_none() {
            continue;
        }
        let line = match *i {
            Input::Move(p) => format!("move({})", xy(p)),
            Input::Click(p, Btn::Left) => format!("click({})", xy(p)),
            Input::Click(p, Btn::Right) => format!("rmb({})", xy(p)),
            Input::Click(_, Btn::Middle) => continue,
            Input::DblClick(p) => format!("dblclick({})", xy(p)),
            Input::Press(p, Btn::Left) => format!("press({})", xy(p)),
            Input::Release(p, Btn::Left) => format!("release({})", xy(p)),
            Input::Press(..) | Input::Release(..) => continue,
            Input::Key(k, _) => format!("key({k:?})"),
            Input::Tick(ms) => {
                let d = ms.saturating_sub(last_tick);
                last_tick = ms;
                format!("tick({d})")
            }
            Input::Mods(m) => {
                let mut v = vec![];
                for (name, was, now) in
                    [("Shift", mods.shift, m.shift), ("Ctrl", mods.ctrl, m.ctrl), ("Alt", mods.alt, m.alt)]
                {
                    if was != now {
                        v.push(format!("mod({name},{})", if now { "down" } else { "up" }));
                    }
                }
                mods = m;
                v.join(" ")
            }
            Input::Char(_) => unreachable!("handled above"),
        };
        if !line.is_empty() {
            out.push(line);
        }
    }
    flush(&mut typed, &mut out);
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_errors_have_position() {
        let cases: [(&str, usize, usize, &str); 8] = [
            ("tool(WA) → wall[0].c=(1,2)", 1, 12, "expected wall[i].a"),
            ("click(1,2)\n  jump(3,4)", 2, 3, "unknown event"),
            ("click(1;2)", 1, 1, "expected (x,y)"),
            ("tool(WA) click(0,0) → walls=x", 1, 23, "expected a count"),
            ("→ walls=1", 1, 1, "needs an event"),
            ("view(sheet)", 1, 1, "not supported"),
            ("click(0,0) type(\"12)", 1, 16, "unclosed"),
            ("tool(XX)", 1, 1, "unknown tool"),
        ];
        for (src, line, col, msg) in cases {
            let e = parse(src).expect_err(src);
            assert_eq!((e.line, e.col), (line, col), "{src}: {e}");
            assert!(e.msg.contains(msg), "{src}: {e}");
        }
    }

    #[test]
    fn parse_expands_events_and_attaches_checks() {
        let s = parse(
            "zoom=50 walls=[(0,0)-(6,0)]\n\
             drag(0,0→1,0) type(\"12\") → selected=0 status^=\"W\"\n\
             hover(1,1,300) key(Space) undo # comment\n\
             SNAP-5 hover(6,0,300) #pending",
        )
        .unwrap();
        assert_eq!(s.setup.zoom, Some(50.0));
        assert_eq!(s.setup.walls, vec![(v2(0.0, 0.0), v2(6.0, 0.0))]);
        // press + 8 moves + release + 2 chars, hover = move + wait, space, undo
        assert_eq!(s.steps.len(), 10 + 2 + 2 + 1 + 1);
        assert_eq!(s.steps[11].1.len(), 2, "checks go to the last char");
        assert_eq!(s.steps[8].0, Ev::In(Input::Move(v2(1.0, 0.0).into())));
        assert_eq!(s.steps[13].0, Ev::Wait(300));
        assert_eq!(s.steps[15].0, Ev::Undo);
    }

    /// Journal rows without the clock column.
    fn journal(ed: &Editor) -> Vec<String> {
        let db = ed.model.sql();
        let cols: Vec<String> = db
            .prepare("SELECT name FROM pragma_table_info('op') WHERE name<>'stamp' ORDER BY cid")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|c| format!("quote({})", c.unwrap()))
            .collect();
        let sql = format!("SELECT {} FROM op ORDER BY seq", cols.join("||'|'||"));
        db.prepare(&sql).unwrap().query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    }

    #[test]
    fn record_then_replay_same_journal() {
        let mut live = Editor::new_in_memory().unwrap();
        live.set_mode(Mode::Polyline);
        record_start(&mut live);
        let ins = [
            Input::Move(v2(0.0, 0.0).into()),
            Input::Click(v2(0.0, 0.0).into(), Btn::Left),
            Input::Move(v2(1.0, 0.02).into()),
            Input::Char('4'),
            Input::Char('5'),
            Input::Char('0'),
            Input::Char('0'),
            Input::Key(Key::Enter, Mods::NONE),
            Input::Tick(40),
            Input::Mods(Mods { shift: true, ..Mods::NONE }),
            Input::Click(v2(4.5, 3.25).into(), Btn::Left),
            Input::Mods(Mods::NONE),
            Input::Key(Key::Escape, Mods::NONE),
            Input::Key(Key::Escape, Mods::NONE),
        ];
        for i in ins {
            Interaction::update(&mut live, i);
        }
        assert!(!live.walls.is_empty());
        let text = record(&live);
        assert!(text.starts_with("tool(arch.wall)\nmove(0,0)\nclick(0,0)"), "{text}");
        assert!(text.contains("type(\"4500\")") && text.contains("tick(40)"), "{text}");
        let s = parse(&text).unwrap();
        let mut again = Editor::new_in_memory().unwrap();
        assert!(run(&mut again, &s).ok());
        assert_eq!(journal(&again), journal(&live));
        assert_eq!(*again.ix.scope(), Scope::Idle);
    }

    #[test]
    fn checks_report_line_want_and_got() {
        let s = parse("tool(WA) click(0,0) click(4,0)\nkey(Escape) → walls=2 state=Idle\nkey(Escape) → state=Idle").unwrap();
        let mut ed = Editor::new_in_memory().unwrap();
        let r = run(&mut ed, &s);
        assert_eq!(r.failures.len(), 2, "{r:?}");
        assert_eq!(r.failures[0], Failure { line: 2, want: "walls=2".into(), got: "walls=1".into() });
        assert_eq!(r.failures[1].line, 2);
        assert_eq!(r.failures[1].want, "Idle");
    }

    #[test]
    fn zoom_sets_the_snap_radius_from_screen_pixels() {
        let mut ed = Editor::new_in_memory().unwrap();
        run(&mut ed, &parse("zoom=10").unwrap());
        assert!((ed.tool.radius - 1.2).abs() < 1e-12);
    }
}
