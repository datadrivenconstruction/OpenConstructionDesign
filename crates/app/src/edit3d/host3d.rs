// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Host commands by picking in 3D: attach the top or base of walls to a roof or floor, and
//! join, unjoin or cut geometry of two elements. The walls of the selection start an attach
//! (more are picked in the tool); the click on the target or the second element is one model
//! step through `Editor::run_op`, so one undo step. The model does the work (`attach_wall_top`,
//! `attach_wall_to_slab`, `join_geometry`, `unjoin_geometry`, `cut_geometry`).

use std::sync::Arc;

use ddc_model::{Model, WallSide};

use super::pick::{Cat, Target};
use crate::editor::OpFn;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    AttachTop,
    AttachBase,
    Join,
    Unjoin,
    /// The first pick cuts the second.
    Cut,
}

impl Op {
    pub const ALL: [Op; 5] = [Op::AttachTop, Op::AttachBase, Op::Join, Op::Unjoin, Op::Cut];

    /// Hint key `e3d.hint.<key>`.
    pub fn key(self) -> &'static str {
        match self {
            Op::AttachTop => "attach_top",
            Op::AttachBase => "attach_base",
            Op::Join => "join",
            Op::Unjoin => "unjoin",
            Op::Cut => "cut",
        }
    }

    pub fn command(self) -> &'static str {
        match self {
            Op::AttachTop => "e3d.attach_top",
            Op::AttachBase => "e3d.attach_base",
            Op::Join => "e3d.join",
            Op::Unjoin => "e3d.unjoin",
            Op::Cut => "e3d.cut",
        }
    }

    fn side(self) -> Option<WallSide> {
        match self {
            Op::AttachTop => Some(WallSide::Top),
            Op::AttachBase => Some(WallSide::Base),
            _ => None,
        }
    }

    /// Status key after a commit.
    fn done(self) -> &'static str {
        match self {
            Op::AttachTop | Op::AttachBase => "e3d.done.attach",
            Op::Join => "e3d.done.join",
            Op::Unjoin => "e3d.done.unjoin",
            Op::Cut => "e3d.done.cut",
        }
    }
}

/// Picks collected by the tool.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    /// Attach: the walls to attach.
    pub walls: Vec<i64>,
    /// Join, unjoin, cut: the first element (the cutter for a cut).
    pub first: Option<Target>,
}

/// What a click on a target does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Attach: the wall joins (or leaves) the walls to attach.
    Wall,
    /// Join/cut: the first element.
    First,
    /// The command runs.
    Commit,
}

/// Element of a target for the model's join table; `None` when it has no joinable body.
pub fn item(t: Target) -> Option<(String, i64)> {
    const KINDS: [&str; 7] = [
        "wall",
        "slab",
        "roof",
        "col",
        "beam",
        "curtain_wall",
        "mass",
    ];
    t.sel()
        .and_then(crate::clip_tools::sel_item)
        .filter(|(k, _)| KINDS.contains(&k.as_str()))
}

impl State {
    /// Picks of the step back; false when there were none.
    pub fn clear(&mut self) -> bool {
        let had = !self.walls.is_empty() || self.first.is_some();
        *self = State::default();
        had
    }

    /// Tool start: the selected walls are the walls of an attach.
    pub fn start(&mut self, op: Op, sel: &[Target]) {
        self.clear();
        if op.side().is_some() {
            self.walls = sel
                .iter()
                .filter(|t| t.cat == Cat::Wall)
                .map(|t| t.id)
                .collect();
        }
    }

    /// Live validation: what a click on `t` would do, or the `e3d.err.*` reason it is refused.
    pub fn check(&self, op: Op, t: Target) -> Result<Step, &'static str> {
        if let Some(side) = op.side() {
            return match t.cat {
                Cat::Wall => Ok(Step::Wall),
                _ if self.walls.is_empty() => Err("e3d.err.attach_walls"),
                Cat::Slab => Ok(Step::Commit),
                Cat::Roof if side == WallSide::Top => Ok(Step::Commit),
                Cat::Roof => Err("e3d.err.attach_base_roof"),
                _ => Err("e3d.err.attach_target"),
            };
        }
        if item(t).is_none() {
            return Err("e3d.err.join_element");
        }
        match self.first {
            None => Ok(Step::First),
            Some(f) if item(f) == item(t) => Err("e3d.err.join_same"),
            Some(_) => Ok(Step::Commit),
        }
    }

    /// The model step of a click that completes the command on `t`.
    fn op(&self, op: Op, t: Target) -> Option<OpFn> {
        if let Some(side) = op.side() {
            let (walls, host, roof) = (self.walls.clone(), t.id, t.cat == Cat::Roof);
            return Some(OpFn(Arc::new(move |m: &mut Model| {
                for &w in &walls {
                    if roof {
                        m.attach_wall_top(w, host)?;
                    } else {
                        m.attach_wall_to_slab(w, side, host)?;
                    }
                }
                Ok(())
            })));
        }
        let (a, b) = (item(self.first?)?, item(t)?);
        Some(OpFn(Arc::new(move |m: &mut Model| {
            let (a, b) = ((a.0.as_str(), a.1), (b.0.as_str(), b.1));
            match op {
                Op::Join => m.join_geometry(a, b),
                Op::Unjoin => m.unjoin_geometry(a, b),
                _ => m.cut_geometry(a, b),
            }
        })))
    }

    /// A click on `t`: Ok(key) is the status key of what happened.
    pub fn click(
        &mut self,
        ed: &mut crate::editor::Editor,
        op: Op,
        t: Target,
    ) -> Result<&'static str, ddc_model::Error> {
        let bad = |k: &str| ddc_model::Error::Invalid(k.into());
        match self.check(op, t).map_err(bad)? {
            Step::Wall => {
                match self.walls.iter().position(|w| *w == t.id) {
                    Some(i) => {
                        self.walls.remove(i);
                    }
                    None => self.walls.push(t.id),
                }
                Ok("e3d.pick.target")
            }
            Step::First => {
                self.first = Some(t);
                Ok("e3d.pick.second")
            }
            Step::Commit => {
                let f = self.op(op, t).ok_or_else(|| bad("e3d.err.join_element"))?;
                ed.run_op(f)?;
                self.clear();
                Ok(op.done())
            }
        }
    }

    /// Targets picked so far (highlighted).
    pub fn picked(&self) -> Vec<Target> {
        let mut v: Vec<Target> = self
            .walls
            .iter()
            .map(|&w| Target::new(Cat::Wall, w))
            .collect();
        v.extend(self.first);
        v
    }
}
