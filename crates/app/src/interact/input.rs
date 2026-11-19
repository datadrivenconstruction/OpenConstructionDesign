// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Abstract input of a tool session. Points are `Pointer`s (a ray and a work plane
//! in model coordinates) from any view; time comes as `Tick` so delays replay deterministically.
use super::pointer::Pointer;
use crate::wall_tool::Event;
use ddc_geom::V2;

pub use crate::wall_tool::Key;

/// A point of the input: the cursor in model coordinates, whatever view it came from.
pub type P = Pointer;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Btn {
    Left,
    Right,
    Middle,
}

/// Modifier keys held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Mods {
    pub const NONE: Mods = Mods { shift: false, ctrl: false, alt: false };
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    Move(P),
    Press(P, Btn),
    Release(P, Btn),
    Click(P, Btn),
    DblClick(P),
    Key(Key, Mods),
    Char(char),
    Mods(Mods),
    /// Clock in ms since an arbitrary start.
    Tick(u64),
}

impl From<Event> for Input {
    fn from(e: Event) -> Self {
        match e {
            Event::Move(p) => Input::Move(p.into()),
            Event::Click(p) => Input::Click(p.into(), Btn::Left),
            Event::Press(p) => Input::Press(p.into(), Btn::Left),
            Event::Release(p) => Input::Release(p.into(), Btn::Left),
            Event::Key(k) => Input::Key(k, Mods::NONE),
            Event::Char(c) => Input::Char(c),
        }
    }
}

impl Input {
    /// The plan event this input stands for; `None` for inputs the plan tools never saw
    /// (other buttons, double click, modifiers, time) and for a pointer that gives no plan
    /// point (a ray along the work plane): such an event is skipped. Inverse of `From<Event>`.
    pub fn event(&self) -> Option<Event> {
        Some(match *self {
            Input::Move(p) => Event::Move(p.plan()?),
            Input::Click(p, Btn::Left) => Event::Click(p.plan()?),
            Input::Press(p, Btn::Left) => Event::Press(p.plan()?),
            Input::Release(p, Btn::Left) => Event::Release(p.plan()?),
            Input::Key(k, _) => Event::Key(k),
            Input::Char(c) => Event::Char(c),
            _ => return None,
        })
    }

    /// The pointer of a pointer input.
    pub fn pointer(&self) -> Option<&Pointer> {
        match self {
            Input::Move(p)
            | Input::Press(p, _)
            | Input::Release(p, _)
            | Input::Click(p, _)
            | Input::DblClick(p) => Some(p),
            _ => None,
        }
    }

    /// The same input with its pointer replaced by `f` (another view of the same point);
    /// inputs without a pointer pass through, `None` from `f` drops the input.
    pub fn map_pointer(self, f: impl FnOnce(Pointer) -> Option<Pointer>) -> Option<Input> {
        Some(match self {
            Input::Move(p) => Input::Move(f(p)?),
            Input::Press(p, b) => Input::Press(f(p)?, b),
            Input::Release(p, b) => Input::Release(f(p)?, b),
            Input::Click(p, b) => Input::Click(f(p)?, b),
            Input::DblClick(p) => Input::DblClick(f(p)?),
            other => other,
        })
    }

    /// Plan point of a pointer input (what plan sessions read); `None` when the view gives none.
    pub fn point(&self) -> Option<V2> {
        self.pointer().and_then(Pointer::plan)
    }
}
