// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `Scope` — what the editor is doing now. One value, owned by `Interaction`;
//! hot set and overlays derive from it. `from_legacy` reads the old flags until 72–76 move them.
use crate::editor::Editor;
use crate::grips::{Kind as GripKind, Role};
use crate::wall_tool::Mode;

/// Ribbon command id of a tool (`arch.wall`); modes without a command use a bare word
/// (`sketch`, `shape`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ToolId(pub &'static str);

/// Modify operation of `Scope::Moving` (the modify operations).
pub type ModOp = crate::modify_tools::Op;

/// What a grip drag or a reshape acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// Model element by grip kind and row id.
    Elem(GripKind, i64),
    /// Section or elevation mark of view `0`.
    View(i64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Scope {
    /// Nothing owns the pointer: clicks select.
    #[default]
    Idle,
    /// A tool collects points; `step` is the hint key (`hint.<tool>.<step>`).
    Drafting { tool: ToolId, step: &'static str },
    /// A tool waits for one click that places or picks.
    Placing { tool: ToolId },
    /// The selection moves under a modify operation.
    Moving { op: ModOp },
    /// A grip is dragged or its temporary dimension is typed; `grip` is `grip_code` of the role.
    GripDrag { target: Target, grip: u32 },
    /// The shape of one element is edited point by point.
    Reshaping { target: Target },
    BoxSelect,
    Painting,
    Measuring,
    Pointing,
}

/// Variant of a `Scope` without data: the row key of `SCOPE_TABLE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScopeKind {
    Idle,
    Drafting,
    Placing,
    Moving,
    GripDrag,
    Reshaping,
    BoxSelect,
    Painting,
    Measuring,
    Pointing,
}

impl ScopeKind {
    pub const ALL: [ScopeKind; 10] = [
        ScopeKind::Idle,
        ScopeKind::Drafting,
        ScopeKind::Placing,
        ScopeKind::Moving,
        ScopeKind::GripDrag,
        ScopeKind::Reshaping,
        ScopeKind::BoxSelect,
        ScopeKind::Painting,
        ScopeKind::Measuring,
        ScopeKind::Pointing,
    ];
}

impl Scope {
    pub fn kind(&self) -> ScopeKind {
        match self {
            Scope::Idle => ScopeKind::Idle,
            Scope::Drafting { .. } => ScopeKind::Drafting,
            Scope::Placing { .. } => ScopeKind::Placing,
            Scope::Moving { .. } => ScopeKind::Moving,
            Scope::GripDrag { .. } => ScopeKind::GripDrag,
            Scope::Reshaping { .. } => ScopeKind::Reshaping,
            Scope::BoxSelect => ScopeKind::BoxSelect,
            Scope::Painting => ScopeKind::Painting,
            Scope::Measuring => ScopeKind::Measuring,
            Scope::Pointing => ScopeKind::Pointing,
        }
    }

    /// Tool of a drafting or placing scope.
    pub fn tool(&self) -> Option<ToolId> {
        match self {
            Scope::Drafting { tool, .. } | Scope::Placing { tool } => Some(*tool),
            _ => None,
        }
    }

    /// Scope the old flags describe, in the priority order of the legacy chain (who would
    /// take the next event). Temporary: shrinks as groups move to sessions.
    pub fn from_legacy(ed: &Editor) -> Scope {
        let draft = |tool, started: bool| Scope::Drafting {
            tool: ToolId(tool),
            step: if started { "next" } else { "first" },
        };
        if let Some(c) = crate::clip_tools::armed_command(ed) {
            return Scope::Placing { tool: ToolId(c) };
        }
        if let Some(m) = &ed.sketch {
            return Scope::Drafting { tool: ToolId("sketch"), step: m.draw.key() };
        }
        if let Some(s) = &ed.shape {
            return Scope::Reshaping { target: Target::Elem(GripKind::Slab, s.slab) };
        }
        if let Some(c) = crate::roof_edge_tools::armed_command(ed) {
            return Scope::Placing { tool: ToolId(c) };
        }
        if crate::annot_text::dragging() {
            return Scope::Moving { op: ModOp::Move };
        }
        if let Some(c) = crate::annot_text::armed_command() {
            return Scope::Placing { tool: ToolId(c) };
        }
        if crate::annot_text::busy() {
            return Scope::Drafting { tool: ToolId("ann.text"), step: "type" };
        }
        if let Some(t) = ed.topo.tool {
            return Scope::Drafting { tool: ToolId(t.command()), step: ed.topo.step() };
        }
        if crate::solid_tools::dragging() {
            return Scope::Moving { op: ModOp::Move };
        }
        if let Some((c, first)) = crate::solid_tools::armed_command(ed) {
            return draft(c, !first);
        }
        if let Some(t) = &ed.mass {
            return Scope::Drafting { tool: ToolId(t.cmd.id()), step: t.step() };
        }
        if ed.opening_mode {
            use crate::opening_tool::Kind;
            let c = match ed.otool.ty.kind {
                Kind::Door => "arch.door",
                Kind::Window => "arch.window",
                Kind::Void => "arch.opening",
            };
            return Scope::Placing { tool: ToolId(c) };
        }
        // the legacy reading: the wall's arm request, not the session (`live` asks this first)
        if !ed.tool_armed() && !ed.tool.armed() {
            if let Some(a) = &ed.grip.active {
                let g = &a.grip;
                return Scope::GripDrag {
                    target: Target::Elem(g.kind, g.id),
                    grip: grip_code(&g.role),
                };
            }
            if let (Some(_), Some((k, id, r))) = (&ed.grip.typing, &ed.grip.last) {
                return Scope::GripDrag { target: Target::Elem(*k, *id), grip: grip_code(r) };
            }
            if let Some(v) = &ed.vdrag {
                return Scope::GripDrag { target: Target::View(v.id), grip: v.grip as u32 };
            }
            if let Some(d) = &ed.drag {
                return Scope::GripDrag {
                    target: Target::Elem(GripKind::Opening, d.id),
                    grip: grip_code(&Role::Move),
                };
            }
        }
        let started = !ed.wall_idle();
        let m = ed.tool.mode;
        match m {
            Mode::Select => Scope::Idle,
            Mode::PickLine => Scope::Drafting {
                tool: ToolId(crate::line_pick::command(&ed.tool)),
                step: crate::line_pick::hint(&ed.tool).1,
            },
            Mode::Polyline if ed.tool.curtain => draft("arch.curtain_wall", started),
            Mode::Polyline => draft("arch.wall", started),
            Mode::Rect => draft("arch.wall_room", started),
        }
    }
}

/// Stable number of a grip role: variant in the high half, index in the low half.
pub fn grip_code(r: &Role) -> u32 {
    let (v, i): (u32, usize) = match r {
        Role::End(k) => (0, *k as usize),
        Role::Flip => (1, 0),
        Role::Vertex(k) => (2, *k),
        Role::Edge(k) => (3, *k),
        Role::Move => (4, 0),
        Role::Rotate => (5, 0),
        Role::Run => (6, 0),
        Role::Elevation => (7, 0),
        Role::Param(_, k) => (8, *k as usize),
        Role::Bulge => (9, 0),
        Role::Z(k) => (10, *k as usize),
        Role::Width => (11, 0),
        Role::Landing => (12, 0),
        Role::LandingDepth => (13, 0),
        Role::Bubble(k) => (14, *k as usize),
        Role::Mode(k) => (15, *k as usize),
        Role::HoleVertex(h, k) => (16, (*h << 8) | *k),
        Role::HoleEdge(h, k) => (17, (*h << 8) | *k),
        Role::Insert(k) => (18, *k),
        Role::FlipHand => (19, 0),
        Role::FlipFacing => (20, 0),
    };
    (v << 16) | (i.min(0xFFFF) as u32)
}
