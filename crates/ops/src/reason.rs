// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Rejections a model can act on (docs/design/AI_NATIVE.md section 2): a closed, stable code,
//! a JSON pointer to the offending field, what was expected and what was got, and the refs
//! involved. People read the translated `hint_key` (`ops.reason.<code>`); models read `code`.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::check::Issue;
use crate::refs::Ref;
use crate::{Envelope, WireError};

/// Closed set of rejection codes. The wire names (`key`) never change once released.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    // wire and structure (`Envelope::from_json`, `Envelope::check`)
    Parse,
    Version,
    NoOps,
    DuplicateGroup,
    DuplicateSlot,
    BadDependency,
    UnknownSlot,
    UndeclaredDependency,
    // model
    NotFound,
    EmptySelection,
    Unsupported,
    Parallel,
    NotParallel,
    BadCount,
    Degenerate,
    OutsideHost,
    OffHost,
    OutOfRange,
    NonFinite,
    BadKind,
    /// A part name (`face:left`, `edge:3`, `mid`) the grammar does not know.
    BadPart,
    TooMany,
    Referenced,
    Conflict,
    Geometry,
    NewerFile,
    Corrupt,
    Internal,
    /// Not yet classified: `got` carries the model's own text.
    Invalid,
}

impl Code {
    pub const ALL: [Code; 29] = [
        Code::Parse,
        Code::Version,
        Code::NoOps,
        Code::DuplicateGroup,
        Code::DuplicateSlot,
        Code::BadDependency,
        Code::UnknownSlot,
        Code::UndeclaredDependency,
        Code::NotFound,
        Code::EmptySelection,
        Code::Unsupported,
        Code::Parallel,
        Code::NotParallel,
        Code::BadCount,
        Code::Degenerate,
        Code::OutsideHost,
        Code::OffHost,
        Code::OutOfRange,
        Code::NonFinite,
        Code::BadKind,
        Code::BadPart,
        Code::TooMany,
        Code::Referenced,
        Code::Conflict,
        Code::Geometry,
        Code::NewerFile,
        Code::Corrupt,
        Code::Internal,
        Code::Invalid,
    ];

    /// Wire name, equal to the serde form.
    pub fn key(self) -> &'static str {
        match self {
            Code::Parse => "parse",
            Code::Version => "version",
            Code::NoOps => "no_ops",
            Code::DuplicateGroup => "duplicate_group",
            Code::DuplicateSlot => "duplicate_slot",
            Code::BadDependency => "bad_dependency",
            Code::UnknownSlot => "unknown_slot",
            Code::UndeclaredDependency => "undeclared_dependency",
            Code::NotFound => "not_found",
            Code::EmptySelection => "empty_selection",
            Code::Unsupported => "unsupported",
            Code::Parallel => "parallel",
            Code::NotParallel => "not_parallel",
            Code::BadCount => "bad_count",
            Code::Degenerate => "degenerate",
            Code::OutsideHost => "outside_host",
            Code::OffHost => "off_host",
            Code::OutOfRange => "out_of_range",
            Code::NonFinite => "non_finite",
            Code::BadKind => "bad_kind",
            Code::BadPart => "bad_part",
            Code::TooMany => "too_many",
            Code::Referenced => "referenced",
            Code::Conflict => "conflict",
            Code::Geometry => "geometry",
            Code::NewerFile => "newer_file",
            Code::Corrupt => "corrupt",
            Code::Internal => "internal",
            Code::Invalid => "invalid",
        }
    }

    pub fn from_key(s: &str) -> Option<Code> {
        Code::ALL.iter().copied().find(|c| c.key() == s)
    }

    /// Locale key of the text shown to people (`crates/ui/data/i18n/*/ops_reason.json`).
    pub fn hint_key(self) -> String {
        format!("ops.reason.{}", self.key())
    }
}

/// Why an envelope or one of its ops was refused.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rejection {
    /// Group id (`Group::id`), when the rejection belongs to one group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u32>,
    /// Op index inside that group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<u32>,
    /// JSON pointer into the envelope, e.g. `/groups/0/ops/3/el/height`; empty = whole envelope.
    pub path: String,
    pub code: Code,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub got: Option<String>,
    pub hint_key: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<Ref>,
}

/// Number for `expected`/`got`: at most 3 decimals, trailing zeros dropped.
pub fn num(x: f64) -> String {
    if !x.is_finite() {
        return format!("{x}");
    }
    let s = format!("{x:.3}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.') } else { &s };
    if s == "-0" {
        "0".to_owned()
    } else {
        s.to_owned()
    }
}

impl Rejection {
    pub fn new(code: Code) -> Rejection {
        Rejection {
            group: None,
            op: None,
            path: String::new(),
            code,
            expected: None,
            got: None,
            hint_key: code.hint_key(),
            refs: Vec::new(),
        }
    }

    /// `expected: "lo..hi unit"`, `got: "x unit"`.
    pub fn out_of_range(field: &str, lo: f64, hi: f64, unit: &str, got: f64) -> Rejection {
        Rejection::new(Code::OutOfRange)
            .field(field)
            .expected(format!("{}..{} {unit}", num(lo), num(hi)))
            .got(format!("{} {unit}", num(got)))
    }

    /// Open upper bound: `expected: ">= lo unit"`.
    pub fn at_least(field: &str, lo: f64, unit: &str, got: f64) -> Rejection {
        Rejection::new(Code::OutOfRange)
            .field(field)
            .expected(format!(">= {} {unit}", num(lo)))
            .got(format!("{} {unit}", num(got)))
    }

    /// Append a field pointer (`/el/height`) to the current path.
    pub fn field(mut self, field: &str) -> Rejection {
        self.path.push_str(field);
        self
    }

    /// Prefix the path, e.g. with the op's payload key (`/el`).
    pub fn under(mut self, prefix: &str) -> Rejection {
        self.path.insert_str(0, prefix);
        self
    }

    /// Place the rejection on op `oi` of group index `gi`: sets `group`/`op` and prefixes
    /// `/groups/{gi}/ops/{oi}` to the field path.
    pub fn in_op(mut self, env: &Envelope, gi: usize, oi: usize) -> Rejection {
        self.group = env.groups.get(gi).map(|g| g.id);
        self.op = u32::try_from(oi).ok();
        self.under(&format!("/groups/{gi}/ops/{oi}"))
    }

    pub fn expected(mut self, s: impl Into<String>) -> Rejection {
        self.expected = Some(s.into());
        self
    }

    pub fn got(mut self, s: impl Into<String>) -> Rejection {
        self.got = Some(s.into());
        self
    }

    pub fn with_ref(mut self, r: Ref) -> Rejection {
        self.refs.push(r);
        self
    }

    /// From a structural issue of `Envelope::check`; group and op come from its path.
    pub fn from_issue(env: &Envelope, issue: &Issue) -> Rejection {
        let mut r = Rejection::new(Code::from_key(issue.code).unwrap_or(Code::Invalid));
        if r.code == Code::Invalid {
            r.got = Some(issue.code.to_owned());
        }
        let mut parts = issue.path.split('/').skip(1);
        if parts.next() == Some("groups") {
            if let Some(gi) = parts.next().and_then(|s| s.parse::<usize>().ok()) {
                r.group = env.groups.get(gi).map(|g| g.id);
                if parts.next() == Some("ops") {
                    r.op = parts.next().and_then(|s| s.parse().ok());
                }
            }
        }
        r.path = issue.path.clone();
        r
    }

    /// From a failed strict parse.
    pub fn from_wire(e: &WireError) -> Rejection {
        match e {
            WireError::Json(j) => Rejection::new(Code::Parse).got(j.to_string()),
            WireError::NoVersion => Rejection::new(Code::Version)
                .field("/v")
                .expected(crate::OP_VERSION.to_string())
                .got("none"),
            WireError::Version { got, want } => Rejection::new(Code::Version)
                .field("/v")
                .expected(want.to_string())
                .got(got.to_string()),
        }
    }
}

impl Envelope {
    /// `check` as rejections; empty when the envelope is structurally sound.
    pub fn rejections(&self) -> Vec<Rejection> {
        self.check().iter().map(|i| Rejection::from_issue(self, i)).collect()
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code.key())?;
        if !self.path.is_empty() {
            write!(f, " at {}", self.path)?;
        }
        if let Some(e) = &self.expected {
            write!(f, ": expected {e}")?;
        }
        if let Some(g) = &self.got {
            write!(f, "{} got {g}", if self.expected.is_some() { "," } else { ":" })?;
        }
        if !self.refs.is_empty() {
            let v: Vec<String> = self.refs.iter().map(Ref::label).collect();
            write!(f, " [{}]", v.join(", "))?;
        }
        Ok(())
    }
}

impl std::error::Error for Rejection {}
