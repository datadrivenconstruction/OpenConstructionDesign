//! The command registry (docs/design/API_REGISTRY.md): every model command, read or edit, with a
//! stable id, a version, typed input and result schemas and an en/ru text key. Chat, external
//! agents, scripts and the command line all reach the model through `call`; a failure is always
//! a coded error, never a silent nothing.

use crate::catalog::{self, Client, Retired, Target, Tier, TierPolicy, ToolSpec};
use crate::refusal::{self, Refusal, RefusalCode};
use crate::convo::{CallCx, Mode, ToolHost, ToolOutcome};
use crate::mcp::tools as mcp_tools;
use crate::J;
use serde_json::json;
use std::sync::OnceLock;

/// Version of the export document; bumped when its own shape changes.
pub const REGISTRY_VERSION: u32 = 1;
/// Prefix of the en/ru description keys: `api.cmd.<id>`.
pub const TEXT_PREFIX: &str = "api.cmd.";
pub const MAX_SHOWN_ID: usize = 64;

pub use crate::catalog::{Answer, Effect, Kind};

/// Where a command is offered. Chat commands are also offered to agents and the command line;
/// the group, draft and file commands need a caller with its own draft, so the chat lacks them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    All,
    AgentOnly,
}

impl Surface {
    pub fn as_str(self) -> &'static str {
        match self {
            Surface::All => "all",
            Surface::AgentOnly => "agent",
        }
    }
}

/// Who is calling: decides which surfaces it reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caller {
    Chat,
    /// An external agent over MCP.
    Agent,
    /// The command line and scripts run outside the chat; they work a draft like an agent.
    Cli,
}

impl Caller {
    pub fn reaches(self, s: Surface) -> bool {
        s == Surface::All || self != Caller::Chat
    }
}

#[derive(Clone, Copy)]
pub struct Command {
    pub spec: &'static ToolSpec,
    pub version: u32,
    pub kind: Kind,
    pub surface: Surface,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Command")
            .field("id", &self.id())
            .field("version", &self.version)
            .field("kind", &self.kind)
            .field("surface", &self.surface)
            .finish()
    }
}

impl Command {
    pub fn id(&self) -> &'static str {
        self.spec.name
    }

    pub fn text_key(&self) -> String {
        format!("{TEXT_PREFIX}{}", self.id())
    }

    pub fn input_schema(&self) -> J {
        (self.spec.input)().to_json()
    }

    pub fn result_schema(&self) -> J {
        let ok = match self.spec.answer {
            Answer::PlanItem => plan_item_schema(),
            Answer::Data => json!({"type": "object", "required": ["untrusted"],
                "properties": {"untrusted": {"const": "data"}}}),
        };
        json!({"oneOf": [ok, error_schema()]})
    }

    pub fn validate(&self, args: &J) -> Result<(), CmdError> {
        catalog::validate(self.spec, args).map_err(|e| CmdError::from_text(&e))
    }
}

/// Every command in a stable order: the chat catalog, then the agent-only commands. Kind,
/// version, effect and answer live on the one definition (`ToolSpec`).
pub fn commands() -> &'static [Command] {
    static ALL: OnceLock<Vec<Command>> = OnceLock::new();
    ALL.get_or_init(|| {
        let make = |spec: &'static ToolSpec, surface| Command {
            spec,
            version: spec.version,
            kind: spec.kind,
            surface,
        };
        let chat = catalog::catalog().iter().map(|s| make(s, Surface::All));
        let here = mcp_tools::only_here().iter().map(|s| make(s, Surface::AgentOnly));
        chat.chain(here).collect()
    })
}

pub fn find(id: &str) -> Option<&'static Command> {
    commands().iter().find(|c| c.id() == id)
}

/// Stable machine codes; the message beside them is for people.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    UnknownCommand,
    InvalidArgs,
    NotAllowed,
    NotFound,
    /// The command was valid but would change nothing: said aloud, never silent.
    NoEffect,
    Conflict,
    Limit,
    Cancelled,
    /// The caller's session is busy (a group is open, the worker is full): try again later.
    Busy,
    Failed,
}

pub const ERROR_CODES: [ErrorCode; 10] = [
    ErrorCode::UnknownCommand,
    ErrorCode::InvalidArgs,
    ErrorCode::NotAllowed,
    ErrorCode::NotFound,
    ErrorCode::NoEffect,
    ErrorCode::Conflict,
    ErrorCode::Limit,
    ErrorCode::Cancelled,
    ErrorCode::Busy,
    ErrorCode::Failed,
];

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::UnknownCommand => "unknown_command",
            ErrorCode::InvalidArgs => "invalid_args",
            ErrorCode::NotAllowed => "not_allowed",
            ErrorCode::NotFound => "not_found",
            ErrorCode::NoEffect => "no_effect",
            ErrorCode::Conflict => "conflict",
            ErrorCode::Limit => "limit",
            ErrorCode::Cancelled => "cancelled",
            ErrorCode::Busy => "busy",
            ErrorCode::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<ErrorCode> {
        ERROR_CODES.into_iter().find(|c| c.as_str() == s)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CmdError {
    pub code: ErrorCode,
    pub message: String,
    /// JSON path of the offending argument, `args.items[2].cat`.
    pub path: Option<String>,
    /// Element references the error is about, `door:7`.
    pub refs: Vec<String>,
    /// The typed refusal when the failing side named one; otherwise `refusal()` derives it.
    pub refusal: Option<Refusal>,
}

impl ErrorCode {
    /// The refusal code of an error that named none.
    /// A stale plan names itself (`AiError::Stale`, a `status: stale` reply), never by wording.
    pub fn refusal_code(self) -> RefusalCode {
        match self {
            ErrorCode::UnknownCommand => RefusalCode::UnknownTool,
            ErrorCode::InvalidArgs => RefusalCode::InvalidArgs,
            ErrorCode::NotAllowed => RefusalCode::NotAllowedInMode,
            ErrorCode::NotFound => RefusalCode::UnknownTarget,
            ErrorCode::NoEffect => RefusalCode::NoEffect,
            ErrorCode::Conflict => RefusalCode::PlanConflict,
            ErrorCode::Limit => RefusalCode::LimitExceeded,
            ErrorCode::Cancelled => RefusalCode::Cancelled,
            ErrorCode::Busy => RefusalCode::Busy,
            ErrorCode::Failed => RefusalCode::Internal,
        }
    }

    /// The registry code a refusal keeps in the older `code` field.
    pub fn of_refusal(c: RefusalCode) -> ErrorCode {
        match c {
            RefusalCode::InvalidArgs | RefusalCode::DraftInvalid | RefusalCode::NameTaken => {
                ErrorCode::InvalidArgs
            }
            RefusalCode::UnknownTarget | RefusalCode::UnknownFamily => ErrorCode::NotFound,
            RefusalCode::NoEffect => ErrorCode::NoEffect,
            RefusalCode::ToolRetired | RefusalCode::UnknownTool => ErrorCode::UnknownCommand,
            RefusalCode::SnapshotsOff
            | RefusalCode::NotAllowedInMode
            | RefusalCode::NeedsConfirmation
            | RefusalCode::MassDeleteBlocked => ErrorCode::NotAllowed,
            RefusalCode::Busy => ErrorCode::Busy,
            RefusalCode::PlanStale | RefusalCode::PlanConflict | RefusalCode::SinceExpired => {
                ErrorCode::Conflict
            }
            RefusalCode::LimitExceeded | RefusalCode::TextTooLarge => ErrorCode::Limit,
            RefusalCode::Cancelled => ErrorCode::Cancelled,
            RefusalCode::SnapshotUnavailable | RefusalCode::RepairStopped | RefusalCode::Internal => {
                ErrorCode::Failed
            }
            // the spine's own codes never reach an AI caller
            _ => ErrorCode::Failed,
        }
    }
}

/// A command id as its static name: how a refusal read back from text names its fixes.
pub fn known_command(name: &str) -> Option<&'static str> {
    find(name).map(Command::id)
}

const NOT_FOUND: &[&str] = &["no element", "no level", "no item", "no option", "not found", "gone"];
const CONFLICT: &[&str] = &["stale", "changed since", "conflict"];
const LIMIT: &[&str] = &["too many", "too large", "limit", "low on", "free disk", "memory is low"];
const NO_EFFECT: &[&str] = &["nothing is selected", "nothing to "];
const INVALID: &[&str] = &["must", "expected", "bad ", "unknown ", "needs ", "is empty", "give "];

impl CmdError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> CmdError {
        let message = message.into();
        CmdError {
            code,
            refs: refs_in(&message),
            path: None,
            message,
            refusal: None,
        }
    }

    pub fn refused(r: Refusal) -> CmdError {
        let message = r.detail.clone().unwrap_or_else(|| refusal::message(&r, "en"));
        let mut e = CmdError::new(ErrorCode::of_refusal(r.code), message);
        e.path = r.arg_of("path").map(str::to_string);
        e.refusal = Some(r);
        e
    }

    /// The typed refusal: the one named, or derived from the code, path and message.
    pub fn refusal(&self) -> Refusal {
        if let Some(r) = &self.refusal {
            return r.clone();
        }
        let mut r = refusal::of(self.code.refusal_code()).detail(self.message.clone());
        if let Some(p) = &self.path {
            r = r.arg("path", p.clone());
        }
        r
    }

    /// How a handler names its failure: `code: message`, which `from_text` reads back exactly.
    pub fn wire(code: ErrorCode, message: impl std::fmt::Display) -> String {
        format!("{}: {message}", code.as_str())
    }

    /// A handler's text: a leading `code: ` (see `wire`) wins. Only older handlers that do not
    /// name a code yet fall through to the reading of their wording.
    pub fn from_text(text: &str) -> CmdError {
        if let Some(r) = refusal::from_wire(text, &known_command) {
            return CmdError::refused(r);
        }
        if let Some((head, rest)) = text.split_once(": ") {
            if let Some(code) = ErrorCode::parse(head) {
                return CmdError::new(code, rest);
            }
        }
        if let Some(code) = ErrorCode::parse(text) {
            return CmdError::new(code, text);
        }
        let lower = text.to_lowercase();
        let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
        // the argument validator always names the path first: `args.x: expected string`
        if text.starts_with("args") {
            if let Some((path, _)) = text.split_once(": ") {
                let mut e = CmdError::new(ErrorCode::InvalidArgs, text);
                e.path = Some(path.to_string());
                return e;
            }
        }
        let code = if lower.starts_with("unknown tool") {
            ErrorCode::UnknownCommand
        } else if has(NO_EFFECT) {
            ErrorCode::NoEffect
        } else if has(NOT_FOUND) {
            ErrorCode::NotFound
        } else if has(CONFLICT) {
            ErrorCode::Conflict
        } else if has(LIMIT) {
            ErrorCode::Limit
        } else if has(INVALID) {
            ErrorCode::InvalidArgs
        } else {
            ErrorCode::Failed
        };
        CmdError::new(code, text)
    }

    /// The wire form, written by hand so the data fence is always the first field.
    pub fn to_text(&self) -> String {
        let mut s = format!(
            "{{\"untrusted\":\"data\",\"error\":{},\"code\":\"{}\"",
            J::String(self.message.clone()),
            self.code.as_str()
        );
        if let Some(p) = &self.path {
            s.push_str(&format!(",\"path\":{}", J::String(p.clone())));
        }
        if !self.refs.is_empty() {
            s.push_str(&format!(",\"refs\":{}", json!(self.refs)));
        }
        let r = refusal::to_wire(&self.refusal(), "en");
        s.push_str(&format!(",\"refused\":{r}"));
        s.push('}');
        s
    }

    pub fn outcome(&self) -> ToolOutcome {
        ToolOutcome::error(self.to_text())
    }
}

/// `cat:id` tokens of a message, in order, without repeats.
fn refs_in(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| c.is_whitespace() || ",;()[]{}\"'".contains(c)) {
        let t = raw.trim_end_matches(['.', ':']);
        let Some((cat, id)) = t.split_once(':') else {
            continue;
        };
        let cat_ok = !cat.is_empty() && cat.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
        let id_ok = !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit());
        if cat_ok && id_ok && !out.iter().any(|r| r == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// Looks the command up and checks surface, permission and arguments: the gate every caller
/// passes. `ceiling` is the highest tier the caller's session may reach.
pub fn gate_as(
    id: &str,
    args: &J,
    caller: Caller,
    ceiling: Tier,
    images: bool,
) -> Result<&'static Command, CmdError> {
    gate_in(catalog::RETIRED.as_slice(), id, args, caller, ceiling, images)
}

/// `gate_as` against a given table of retired names.
pub fn gate_in(
    retired: &[Retired],
    id: &str,
    args: &J,
    caller: Caller,
    ceiling: Tier,
    images: bool,
) -> Result<&'static Command, CmdError> {
    if let Some(r) = catalog::retired_refusal(retired, id) {
        return Err(CmdError::refused(r));
    }
    let shown: String = id.chars().filter(|c| !c.is_control()).take(MAX_SHOWN_ID).collect();
    let unknown = || CmdError::new(ErrorCode::UnknownCommand, format!("unknown tool: {shown}"));
    let cmd = find(id).ok_or_else(unknown)?;
    if !caller.reaches(cmd.surface) {
        return Err(unknown());
    }
    if !catalog::allowed_at(cmd.spec, ceiling, images) {
        let r = refusal::of(RefusalCode::NotAllowedInMode)
            .arg("tool", cmd.id())
            .detail("not_allowed");
        return Err(CmdError::refused(r));
    }
    cmd.validate(args)?;
    Ok(cmd)
}

/// The highest tier a chat or command-line session reaches in this mode (the catalog's rule).
pub fn mode_ceiling(mode: Mode) -> Tier {
    catalog::ceiling(Client::Chat, mode, TierPolicy::default(), Target::Main)
}

/// The chat's gate.
pub fn gate(
    id: &str,
    args: &J,
    cx: &CallCx<'_>,
    images: bool,
) -> Result<&'static Command, CmdError> {
    gate_as(id, args, Caller::Chat, mode_ceiling(cx.mode), images)
}

/// The one path to the model for the chat.
pub fn call(
    host: &mut dyn ToolHost,
    id: &str,
    args: &J,
    cx: &CallCx<'_>,
    images: bool,
) -> ToolOutcome {
    call_as(host, id, args, cx, images, Caller::Chat)
}

/// The one path to the model for any caller: the host runs only what passed the gate, and
/// whatever it answers on failure carries a code.
pub fn call_as(
    host: &mut dyn ToolHost,
    id: &str,
    args: &J,
    cx: &CallCx<'_>,
    images: bool,
    caller: Caller,
) -> ToolOutcome {
    if cx.cancel.is_cancelled() {
        return localize(CmdError::new(ErrorCode::Cancelled, "cancelled").outcome(), cx.lang);
    }
    let out = match gate_as(id, args, caller, mode_ceiling(cx.mode), images) {
        Ok(cmd) => ensure_coded(host.call(cmd.id(), args, cx)),
        Err(e) => e.outcome(),
    };
    localize(out, cx.lang)
}

const REFUSED_KEY: &str = ",\"refused\":";

/// Error texts are written in English; the caller's session language replaces the trailing
/// `refused` object's texts. Everything before it, the fence first, stays byte for byte.
pub fn localize(mut out: ToolOutcome, lang: &str) -> ToolOutcome {
    if !out.is_error || refusal::lang(lang) == "en" {
        return out;
    }
    let Some(at) = out.text.rfind(REFUSED_KEY) else {
        return out;
    };
    let tail = out.text[at + REFUSED_KEY.len()..].trim_end();
    let Some(obj) = tail.strip_suffix('}') else {
        return out;
    };
    let Some(r) = serde_json::from_str::<J>(obj).ok().and_then(|v| refusal::from_json(&v, &known_command)) else {
        return out;
    };
    out.text = format!("{}{REFUSED_KEY}{}}}", &out.text[..at], refusal::to_wire(&r, lang));
    out
}

/// `ensure_coded`, with the refusal texts in the session language.
pub fn ensure_coded_in(out: ToolOutcome, lang: &str) -> ToolOutcome {
    localize(ensure_coded(out), lang)
}

/// A failed outcome without a code gets one; its other fields stay as the host wrote them.
pub fn ensure_coded(mut out: ToolOutcome) -> ToolOutcome {
    if !out.is_error {
        return out;
    }
    let v: J = serde_json::from_str(&out.text).unwrap_or(J::Null);
    let Some(o) = v.as_object().filter(|_| out.text.trim_end().ends_with('}')) else {
        out.text = CmdError::from_text(&out.text).to_text();
        return out;
    };
    // a host's own `code` and `refused` fields are left as they are rather than written twice
    let (has_code, has_refused) = (o.contains_key("code"), o.contains_key("refused"));
    if has_code && has_refused {
        return out;
    }
    let mut err = match error_of(&out).filter(|_| has_code) {
        Some(e) => e,
        None => {
            let said = ["error", "status", "reason"].iter().find_map(|k| o.get(*k)?.as_str());
            CmdError::from_text(said.unwrap_or_default())
        }
    };
    if err.refusal.is_none() {
        if let Some(code) = o.get("status").and_then(J::as_str).and_then(status_refusal) {
            err.refusal = Some(refusal::of(code).detail(err.message.clone()));
        }
    }
    let mut add: Vec<String> = Vec::new();
    if !has_code {
        add.push(format!("\"code\":\"{}\"", err.code.as_str()));
    }
    if !has_refused {
        add.push(format!("\"refused\":{}", refusal::to_wire(&err.refusal(), "en")));
    }
    let trimmed = out.text.trim_end();
    let body = trimmed.strip_suffix('}').unwrap_or(trimmed).to_string();
    let sep = if o.is_empty() { "" } else { "," };
    out.text = format!("{body}{sep}{}}}", add.join(","));
    out
}

/// The parsed error of a failed outcome; plain text from an older host is coded here too.
/// A reply's own `status` that names its refusal exactly.
fn status_refusal(status: &str) -> Option<RefusalCode> {
    match status {
        "stale" => Some(RefusalCode::PlanStale),
        "busy" => Some(RefusalCode::Busy),
        _ => None,
    }
}

pub fn error_of(out: &ToolOutcome) -> Option<CmdError> {
    if !out.is_error {
        return None;
    }
    let v: J = serde_json::from_str(&out.text).unwrap_or(J::Null);
    let Some(code) = v["code"].as_str().and_then(ErrorCode::parse) else {
        let msg = v["error"].as_str().unwrap_or(&out.text);
        return Some(CmdError::from_text(msg));
    };
    let strs = |k: &str| -> Vec<String> {
        let list = v[k].as_array().map(Vec::as_slice).unwrap_or_default();
        list.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()
    };
    Some(CmdError {
        code,
        message: ["error", "status", "reason"]
            .iter()
            .find_map(|k| v[*k].as_str())
            .unwrap_or(code.as_str())
            .to_string(),
        path: v["path"].as_str().map(str::to_string),
        refs: strs("refs"),
        refusal: v
            .get("refused")
            .and_then(|r| refusal::from_json(r, &known_command))
            .or_else(|| v["status"].as_str().and_then(status_refusal).map(refusal::of)),
    })
}

pub fn error_schema() -> J {
    let codes: Vec<&str> = ERROR_CODES.iter().map(|c| c.as_str()).collect();
    // `refused` (the typed refusal, `refusal::Refusal::to_wire`) rides beside these fields; it
    // stays out of this schema so the frozen command results do not change shape
    json!({
        "type": "object",
        "required": ["untrusted", "error", "code"],
        "properties": {
            "untrusted": {"const": "data"},
            "error": {"type": "string"},
            "code": {"enum": codes},
            "path": {"type": "string"},
            "refs": {"type": "array", "items": {"type": "string"}}
        }
    })
}

fn plan_item_schema() -> J {
    let count = json!({"type": "integer", "minimum": 0});
    json!({
        "type": "object",
        "required": ["untrusted", "item", "changes"],
        "properties": {
            "untrusted": {"const": "data"},
            "item": {"type": "integer", "minimum": 1},
            "changes": {
                "type": "object",
                "required": ["created", "changed", "deleted"],
                "properties": {"created": count, "changed": count, "deleted": count}
            },
            "created": {"type": "array", "items": {"type": "string"}},
            "skipped": {"type": "array", "items": {"type": "string"}}
        }
    })
}

/// The whole registry as one stable JSON document for agents, scripts and the command line.
pub fn export() -> J {
    let cmds: Vec<J> = commands()
        .iter()
        .map(|c| {
            json!({
                "id": c.id(),
                "version": c.version,
                "kind": c.kind.as_str(),
                "surface": c.surface.as_str(),
                "read_only": c.spec.read_only(),
                "destructive": c.spec.destructive(),
                "text": c.text_key(),
                "summary": c.spec.summary,
                "input": c.input_schema(),
                "result": c.result_schema(),
            })
        })
        .collect();
    json!({"registry": REGISTRY_VERSION, "error": error_schema(), "commands": cmds})
}
