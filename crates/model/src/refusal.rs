// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Tool refusal shared by the interaction spine and the AI tool catalogue:
//! a stable code, an i18n message key with arguments and fixes that name a command by id.
//! The name `Refusal` at the crate root belongs to `ai_revert`; this one is `refusal::Refusal`.
use serde::Serialize;

/// Stable reason of a refusal. `as_str` is part of the AI contract: never renamed.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    /// The input cannot be read as the step expects (unparsable value, wrong point count).
    InvalidInput,
    /// The step needs an element under the pointer or in the selection and there is none.
    NoTarget,
    /// The element or the view does not allow the change (pinned, read-only, other level).
    Blocked,
    // AI tool codes: what an agent's call is refused with.
    InvalidArgs,
    UnknownTarget,
    SnapshotsOff,
    SnapshotUnavailable,
    ToolRetired,
    NotAllowedInMode,
    NeedsConfirmation,
    Busy,
    PlanStale,
    PlanConflict,
    MassDeleteBlocked,
    SinceExpired,
    LimitExceeded,
    Cancelled,
    Internal,
    // reserved for AI family drafts
    DraftInvalid,
    TextTooLarge,
    UnknownFamily,
    RepairStopped,
    NameTaken,
    /// A valid call that would change nothing.
    NoEffect,
    /// No tool has the name called.
    UnknownTool,
    // Tool refusal families.
    WallTooShort,
    OpeningNoWall,
    OpeningOutside,
    OpeningTooTall,
    OpeningOverlap,
    OpeningWiderThanWall,
    LevelNoneAbove,
    ComponentNoLibrary,
    XformNotAllowed,
    ConstraintConflict,
}

impl RefusalCode {
    /// Every code, in declaration order.
    pub const ALL: &'static [RefusalCode] = &[
        RefusalCode::InvalidInput,
        RefusalCode::NoTarget,
        RefusalCode::Blocked,
        RefusalCode::InvalidArgs,
        RefusalCode::UnknownTarget,
        RefusalCode::SnapshotsOff,
        RefusalCode::SnapshotUnavailable,
        RefusalCode::ToolRetired,
        RefusalCode::NotAllowedInMode,
        RefusalCode::NeedsConfirmation,
        RefusalCode::Busy,
        RefusalCode::PlanStale,
        RefusalCode::PlanConflict,
        RefusalCode::MassDeleteBlocked,
        RefusalCode::SinceExpired,
        RefusalCode::LimitExceeded,
        RefusalCode::Cancelled,
        RefusalCode::Internal,
        RefusalCode::DraftInvalid,
        RefusalCode::TextTooLarge,
        RefusalCode::UnknownFamily,
        RefusalCode::RepairStopped,
        RefusalCode::NameTaken,
        RefusalCode::NoEffect,
        RefusalCode::UnknownTool,
        RefusalCode::WallTooShort,
        RefusalCode::OpeningNoWall,
        RefusalCode::OpeningOutside,
        RefusalCode::OpeningTooTall,
        RefusalCode::OpeningOverlap,
        RefusalCode::OpeningWiderThanWall,
        RefusalCode::LevelNoneAbove,
        RefusalCode::ComponentNoLibrary,
        RefusalCode::XformNotAllowed,
        RefusalCode::ConstraintConflict,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RefusalCode::InvalidInput => "invalid_input",
            RefusalCode::NoTarget => "no_target",
            RefusalCode::Blocked => "blocked",
            RefusalCode::InvalidArgs => "invalid_args",
            RefusalCode::UnknownTarget => "unknown_target",
            RefusalCode::SnapshotsOff => "snapshots_off",
            RefusalCode::SnapshotUnavailable => "snapshot_unavailable",
            RefusalCode::ToolRetired => "tool_retired",
            RefusalCode::NotAllowedInMode => "not_allowed_in_mode",
            RefusalCode::NeedsConfirmation => "needs_confirmation",
            RefusalCode::Busy => "busy",
            RefusalCode::PlanStale => "plan_stale",
            RefusalCode::PlanConflict => "plan_conflict",
            RefusalCode::MassDeleteBlocked => "mass_delete_blocked",
            RefusalCode::SinceExpired => "since_expired",
            RefusalCode::LimitExceeded => "limit_exceeded",
            RefusalCode::Cancelled => "cancelled",
            RefusalCode::Internal => "internal",
            RefusalCode::DraftInvalid => "draft_invalid",
            RefusalCode::TextTooLarge => "text_too_large",
            RefusalCode::UnknownFamily => "unknown_family",
            RefusalCode::RepairStopped => "repair_stopped",
            RefusalCode::NameTaken => "name_taken",
            RefusalCode::NoEffect => "no_effect",
            RefusalCode::UnknownTool => "unknown_tool",
            RefusalCode::WallTooShort => "wall_too_short",
            RefusalCode::OpeningNoWall => "opening_no_wall",
            RefusalCode::OpeningOutside => "opening_outside",
            RefusalCode::OpeningTooTall => "opening_too_tall",
            RefusalCode::OpeningOverlap => "opening_overlap",
            RefusalCode::OpeningWiderThanWall => "opening_wider_than_wall",
            RefusalCode::LevelNoneAbove => "level_none_above",
            RefusalCode::ComponentNoLibrary => "component_no_library",
            RefusalCode::XformNotAllowed => "xform_not_allowed",
            RefusalCode::ConstraintConflict => "constraint_conflict",
        }
    }

    /// The codes interactive tools refuse with, a subset of `ALL`.
    pub const CODES: &'static [RefusalCode] = &[
        RefusalCode::WallTooShort,
        RefusalCode::OpeningNoWall,
        RefusalCode::OpeningOutside,
        RefusalCode::OpeningTooTall,
        RefusalCode::OpeningOverlap,
        RefusalCode::OpeningWiderThanWall,
        RefusalCode::LevelNoneAbove,
        RefusalCode::ComponentNoLibrary,
        RefusalCode::XformNotAllowed,
        RefusalCode::ConstraintConflict,
    ];

    pub fn parse(s: &str) -> Option<RefusalCode> {
        Self::ALL.iter().copied().find(|c| c.as_str() == s)
    }
}

/// The refusal codes of interactive tools (same as `RefusalCode::CODES`).
pub const CODES: &[RefusalCode] = RefusalCode::CODES;

/// A way out: run command `command` (ribbon id) with `args`; the label is an i18n key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Fix {
    pub label_key: &'static str,
    pub command: &'static str,
    pub args: Vec<(String, String)>,
}

/// Why a tool did not do what was asked, in a form both the UI and the AI read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Refusal {
    pub code: RefusalCode,
    pub msg_key: &'static str,
    pub args: Vec<(&'static str, String)>,
    pub fixes: Vec<Fix>,
    /// The failing component's own words (an argument path, a model error); never translated.
    pub detail: Option<String>,
}

impl Refusal {
    pub fn new(code: RefusalCode, msg_key: &'static str) -> Self {
        Self { code, msg_key, args: vec![], fixes: vec![], detail: None }
    }

    pub fn arg(mut self, k: &'static str, v: impl Into<String>) -> Self {
        self.args.push((k, v.into()));
        self
    }

    pub fn fix(mut self, f: Fix) -> Self {
        self.fixes.push(f);
        self
    }

    /// Empty words are no detail.
    pub fn detail(mut self, d: impl Into<String>) -> Self {
        let d = d.into();
        self.detail = (!d.is_empty()).then_some(d);
        self
    }

    pub fn arg_of(&self, k: &str) -> Option<&str> {
        self.args.iter().find(|(key, _)| *key == k).map(|(_, v)| v.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_snake_case_and_parse_back() {
        let mut seen = std::collections::HashSet::new();
        for &c in RefusalCode::ALL {
            let s = c.as_str();
            assert!(seen.insert(s), "duplicate code {s}");
            assert!(s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{s}");
            assert_eq!(RefusalCode::parse(s), Some(c));
            // the JSON form the AI reads is the same string
            assert_eq!(serde_json::to_string(&c).unwrap(), format!("\"{s}\""));
        }
        assert_eq!(RefusalCode::parse("nope"), None);
    }

    #[test]
    fn builder_keeps_args_and_fixes_in_order() {
        let r = Refusal::new(RefusalCode::NoTarget, "ix.refuse.no_host")
            .arg("kind", "door")
            .arg("level", "1")
            .fix(Fix { label_key: "ix.fix.pick_wall", command: "arch.wall", args: vec![] });
        assert_eq!(r.args, vec![("kind", "door".to_string()), ("level", "1".to_string())]);
        assert_eq!(r.fixes[0].command, "arch.wall");
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["code"], "no_target");
    }
}
