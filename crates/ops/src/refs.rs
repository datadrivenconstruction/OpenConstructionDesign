//! Stable references to model things and target sets.

use schemars::JsonSchema;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Element category. The wire name (`Cat::key`) is the stable prefix of `cat:id` labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Cat {
    Wall,
    Door,
    Window,
    Opening,
    CurtainWall,
    CurtainPanel,
    Slab,
    Roof,
    Ceiling,
    Column,
    Beam,
    Brace,
    Truss,
    BeamSystem,
    Footing,
    StripFooting,
    Raft,
    Stair,
    Railing,
    Ramp,
    Component,
    Room,
    Zone,
    Shaft,
    MepRun,
    MepDevice,
    Rebar,
    Solid,
    Mass,
    Mesh,
    Topography,
    /// Freeform NURBS surface or shell.
    Freeform,
    /// Curtain system of panels and mullions on a freeform surface.
    PanelSystem,
    /// Roof or slab hosted on a freeform surface.
    SurfaceShell,
    /// Diagrid, gridshell or cable net on a host surface.
    Gridshell,
    Level,
    Grid,
    RefPlane,
    Annotation,
    Group,
    /// v1 type ref; read as a wall type (the only v1 meaning). New envelopes name the table.
    Type,
    #[serde(rename = "wtype")]
    WallType,
    #[serde(rename = "stype")]
    SlabType,
    #[serde(rename = "mtype")]
    MemberType,
    Family,
    Param,
    OptionSet,
    DesignOption,
}

impl Cat {
    pub const ALL: [Cat; 48] = [
        Cat::Wall,
        Cat::Door,
        Cat::Window,
        Cat::Opening,
        Cat::CurtainWall,
        Cat::CurtainPanel,
        Cat::Slab,
        Cat::Roof,
        Cat::Ceiling,
        Cat::Column,
        Cat::Beam,
        Cat::Brace,
        Cat::Truss,
        Cat::BeamSystem,
        Cat::Footing,
        Cat::StripFooting,
        Cat::Raft,
        Cat::Stair,
        Cat::Railing,
        Cat::Ramp,
        Cat::Component,
        Cat::Room,
        Cat::Zone,
        Cat::Shaft,
        Cat::MepRun,
        Cat::MepDevice,
        Cat::Rebar,
        Cat::Solid,
        Cat::Mass,
        Cat::Mesh,
        Cat::Topography,
        Cat::Freeform,
        Cat::PanelSystem,
        Cat::SurfaceShell,
        Cat::Gridshell,
        Cat::Level,
        Cat::Grid,
        Cat::RefPlane,
        Cat::Annotation,
        Cat::Group,
        Cat::Type,
        Cat::WallType,
        Cat::SlabType,
        Cat::MemberType,
        Cat::Family,
        Cat::Param,
        Cat::OptionSet,
        Cat::DesignOption,
    ];

    /// Same string as the serde name.
    pub fn key(self) -> &'static str {
        match self {
            Cat::Wall => "wall",
            Cat::Door => "door",
            Cat::Window => "window",
            Cat::Opening => "opening",
            Cat::CurtainWall => "curtain_wall",
            Cat::CurtainPanel => "curtain_panel",
            Cat::Slab => "slab",
            Cat::Roof => "roof",
            Cat::Ceiling => "ceiling",
            Cat::Column => "column",
            Cat::Beam => "beam",
            Cat::Brace => "brace",
            Cat::Truss => "truss",
            Cat::BeamSystem => "beam_system",
            Cat::Footing => "footing",
            Cat::StripFooting => "strip_footing",
            Cat::Raft => "raft",
            Cat::Stair => "stair",
            Cat::Railing => "railing",
            Cat::Ramp => "ramp",
            Cat::Component => "component",
            Cat::Room => "room",
            Cat::Zone => "zone",
            Cat::Shaft => "shaft",
            Cat::MepRun => "mep_run",
            Cat::MepDevice => "mep_device",
            Cat::Rebar => "rebar",
            Cat::Solid => "solid",
            Cat::Mass => "mass",
            Cat::Mesh => "mesh",
            Cat::Topography => "topography",
            Cat::Freeform => "freeform",
            Cat::PanelSystem => "panel_system",
            Cat::SurfaceShell => "surface_shell",
            Cat::Gridshell => "gridshell",
            Cat::Level => "level",
            Cat::Grid => "grid",
            Cat::RefPlane => "ref_plane",
            Cat::Annotation => "annotation",
            Cat::Group => "group",
            Cat::Type => "type",
            Cat::WallType => "wtype",
            Cat::SlabType => "stype",
            Cat::MemberType => "mtype",
            Cat::Family => "family",
            Cat::Param => "param",
            Cat::OptionSet => "option_set",
            Cat::DesignOption => "design_option",
        }
    }

    pub fn from_key(s: &str) -> Option<Cat> {
        Cat::ALL.iter().copied().find(|c| c.key() == s)
    }
}

/// Reference to something in the model or created earlier in the same envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
pub enum Ref {
    /// Existing row: `wall:55`.
    Id { cat: Cat, id: i64 },
    /// Created by an earlier op of this envelope (its `slot`).
    New { slot: u32 },
    /// Frozen selection set by name.
    Set { name: String },
    /// By unique name within the category (levels, grids, types, families).
    Name { cat: Cat, name: String },
    /// Named face, edge or axis of another ref: `wall:55/face:left`, `slab:9/edge:3`.
    Part { of: Box<Ref>, part: String },
}

impl Ref {
    pub fn id(cat: Cat, id: i64) -> Ref {
        Ref::Id { cat, id }
    }
    pub fn slot(slot: u32) -> Ref {
        Ref::New { slot }
    }
    pub fn name(cat: Cat, name: &str) -> Ref {
        Ref::Name { cat, name: name.to_owned() }
    }
    pub fn part(of: Ref, part: &str) -> Ref {
        Ref::Part { of: Box::new(of), part: part.to_owned() }
    }

    /// The ref a `Part` chain hangs on.
    pub fn root(&self) -> &Ref {
        match self {
            Ref::Part { of, .. } => of.root(),
            r => r,
        }
    }

    /// Compact label used in cards and rejections: the canonical text form (see `Display`).
    pub fn label(&self) -> String {
        self.to_string()
    }

    /// Parse the text form: `wall:55`, `new:3`, `set:north`, `grid:A`, `level:"Level 1"`,
    /// `wall:55/face:left`, `slab:9/edge:3/end:a`. A bare all-digit value is an id; a name that
    /// is not a plain token is quoted. Part names are checked and stored in canonical form.
    pub fn parse(text: &str) -> Result<Ref, RefParseError> {
        let err = |at: usize, why: &'static str| RefParseError { text: text.to_owned(), at, why };
        let b = text.as_bytes();
        let colon = text.find(':').ok_or_else(|| err(0, "expected `cat:`"))?;
        let key = &text[..colon];
        let mut i = colon + 1;
        let (raw, quoted) = if b.get(i) == Some(&b'"') {
            let (s, next) = unquote(text, i).ok_or_else(|| err(i, "unterminated quote"))?;
            i = next;
            (s, true)
        } else {
            let end = text[i..].find('/').map_or(text.len(), |k| i + k);
            let s = text[i..end].to_owned();
            if s.is_empty() {
                return Err(err(i, "empty value"));
            }
            i = end;
            (s, false)
        };
        let mut r = match key {
            "new" => {
                if quoted || !raw.bytes().all(|c| c.is_ascii_digit()) {
                    return Err(err(colon + 1, "slot must be a number"));
                }
                Ref::New { slot: raw.parse().map_err(|_| err(colon + 1, "slot out of range"))? }
            }
            "set" => {
                if !quoted && !is_bare(&raw) {
                    return Err(err(colon + 1, "name needs quotes"));
                }
                Ref::Set { name: raw }
            }
            _ => {
                let cat = Cat::from_key(key).ok_or_else(|| err(0, "unknown category"))?;
                if quoted {
                    Ref::Name { cat, name: raw }
                } else if let Some(id) = parse_id(&raw) {
                    Ref::Id { cat, id }
                } else if is_bare(&raw) {
                    Ref::Name { cat, name: raw }
                } else {
                    return Err(err(colon + 1, "name needs quotes"));
                }
            }
        };
        while i < text.len() {
            if b[i] != b'/' {
                return Err(err(i, "expected `/` before a part"));
            }
            i += 1;
            let end = text[i..].find('/').map_or(text.len(), |k| i + k);
            let part = crate::parts::PartName::parse(&text[i..end]).map_err(|why| err(i, why))?;
            r = Ref::Part { of: Box::new(r), part: part.to_string() };
            i = end;
        }
        Ok(r)
    }
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ref::Id { cat, id } => write!(f, "{}:{}", cat.key(), id),
            Ref::New { slot } => write!(f, "new:{slot}"),
            Ref::Set { name } => write!(f, "set:{}", quote_if_needed(name)),
            Ref::Name { cat, name } => write!(f, "{}:{}", cat.key(), quote_if_needed(name)),
            Ref::Part { of, part } => write!(f, "{of}/{part}"),
        }
    }
}

impl FromStr for Ref {
    type Err = RefParseError;
    fn from_str(s: &str) -> Result<Ref, RefParseError> {
        Ref::parse(s)
    }
}

/// Why a ref text did not parse; `at` is a byte offset into `text`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("bad ref `{text}` at {at}: {why}")]
pub struct RefParseError {
    pub text: String,
    pub at: usize,
    pub why: &'static str,
}

/// Only ASCII digits (with an optional minus) are an id, so `+3` or ` 3` stay names.
fn parse_id(s: &str) -> Option<i64> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// A name printed without quotes: ASCII letters, digits, `_ . -`, and not readable as an id.
fn is_bare(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
        && parse_id(s).is_none()
}

fn quote_if_needed(s: &str) -> String {
    if is_bare(s) {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Reads a quoted string starting at byte `at` (the opening quote); returns it and the byte
/// offset after the closing quote.
fn unquote(text: &str, at: usize) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut esc = false;
    for (k, c) in text[at + 1..].char_indices() {
        if esc {
            out.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '"' {
            return Some((out, at + 1 + k + 1));
        } else {
            out.push(c);
        }
    }
    None
}

/// What an op acts on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
pub enum Targets {
    Refs { refs: Vec<Ref> },
    /// Frozen selection set by name.
    Set { name: String },
    /// Filter DSL of `query_elements`, evaluated when the envelope is applied.
    Filter { cat: Option<Cat>, query: String },
}

impl Targets {
    pub fn one(r: Ref) -> Targets {
        Targets::Refs { refs: vec![r] }
    }
}
