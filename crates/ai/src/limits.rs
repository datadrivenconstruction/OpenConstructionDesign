// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Every limit the assistant can meet, declared once: value, unit, reason and tier.
//! Code reads its constants from here; tool descriptions append the limits of their tool;
//! a reply that hits one says so (`truncated`, `Limit::refusal`). Setting limits take the
//! person's value from Settings > AI (`set_overrides`); guards are fixed.

use serde_json::{json, Value as J};
use std::collections::BTreeMap;
use std::sync::RwLock;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Count,
    Chars,
    Bytes,
    MiB,
    Seconds,
    Millis,
    Pixels,
    Metres,
}

impl Unit {
    pub fn word(self) -> &'static str {
        match self {
            Unit::Count => "",
            Unit::Chars => "characters",
            Unit::Bytes => "bytes",
            Unit::MiB => "MB",
            Unit::Seconds => "s",
            Unit::Millis => "ms",
            Unit::Pixels => "px",
            Unit::Metres => "m",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// The person may change it in Settings > AI within `min..=max`.
    Setting { min: u64, max: u64 },
    /// A turn or plan limit the project sets (policy `limits.*`, `plan.*`) or lifts with the
    /// mode chip; the global settings list does not show it.
    Project { min: u64, max: u64 },
    /// Memory or loop safety: fixed.
    Guard,
}

#[derive(Debug)]
pub struct Limit {
    /// Stable id: settings key `ai.limit.<key>`, catalog keys `ai_limits.<key>`.
    pub key: &'static str,
    pub value: u64,
    pub unit: Unit,
    pub reason: &'static str,
    pub tier: Tier,
    /// Tools whose description carries `doc`.
    pub tools: &'static [&'static str],
    /// What a tool description says; `{n}` is the value in force.
    pub doc: &'static str,
    /// The constants that read this limit, as `path/from/crates.rs::NAME`.
    pub src: &'static [&'static str],
}

static OVERRIDES: RwLock<BTreeMap<String, u64>> = RwLock::new(BTreeMap::new());

impl Limit {
    pub const fn n(&self) -> u64 {
        self.value
    }
    pub const fn us(&self) -> usize {
        self.value as usize
    }
    pub const fn u32v(&self) -> u32 {
        self.value as u32
    }
    pub const fn u8v(&self) -> u8 {
        self.value as u8
    }
    pub const fn f(&self) -> f64 {
        self.value as f64
    }
    pub const fn dur(&self) -> Duration {
        match self.unit {
            Unit::Millis => Duration::from_millis(self.value),
            _ => Duration::from_secs(self.value),
        }
    }
    /// The highest value this limit can take: a setting's upper bound, else the value.
    /// Input schemas accept up to here; the value in force refuses by name above it.
    pub const fn hi(&self) -> u64 {
        match self.tier {
            Tier::Setting { max, .. } | Tier::Project { max, .. } => max,
            Tier::Guard => self.value,
        }
    }
    pub fn is_setting(&self) -> bool {
        matches!(self.tier, Tier::Setting { .. })
    }
    /// The value in force: a setting's override (kept in bounds) or the default.
    pub fn get(&self) -> u64 {
        let Tier::Setting { min, max } = self.tier else {
            return self.value;
        };
        let o = OVERRIDES.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        o.get(self.key).map_or(self.value, |v| (*v).clamp(min, max))
    }
    pub fn get_us(&self) -> usize {
        self.get() as usize
    }
    pub fn get_dur(&self) -> Duration {
        match self.unit {
            Unit::Millis => Duration::from_millis(self.get()),
            _ => Duration::from_secs(self.get()),
        }
    }
    /// `200` or `120 MB`.
    pub fn shown(&self) -> String {
        match self.unit.word() {
            "" => self.get().to_string(),
            w => format!("{} {w}", self.get()),
        }
    }
    /// What to do when the limit is hit.
    pub fn remedy(&self) -> &'static str {
        if self.is_setting() {
            "split the work into smaller calls, or ask the person to raise it in Settings > AI"
        } else {
            "split the work into smaller calls or page through it"
        }
    }
    /// A refusal naming the limit, why it exists and what to do.
    pub fn refusal(&self, asked: impl std::fmt::Display) -> String {
        format!(
            "{asked} is over the limit `{}` of {} ({}); {}",
            self.key,
            self.shown(),
            self.reason,
            self.remedy()
        )
    }
}

macro_rules! limits {
    ($($id:ident = $key:literal, $v:expr, $unit:ident, $tier:expr, $reason:literal,
        tools: [$($t:expr),*], doc: $doc:literal, src: [$($s:literal),*];)*) => {
        $(pub const $id: Limit = Limit {
            key: $key, value: $v, unit: Unit::$unit, reason: $reason, tier: $tier,
            tools: &[$($t),*], doc: $doc, src: &[$($s),*],
        };)*
        /// Every limit, in declaration order.
        pub static ALL: &[&Limit] = &[$(&$id),*];
    };
}

const G: Tier = Tier::Guard;
const fn set(min: u64, max: u64) -> Tier {
    Tier::Setting { min, max }
}
const fn project(min: u64, max: u64) -> Tier {
    Tier::Project { min, max }
}

limits! {
    // settings: the person may raise or lower them
    ATTACH_FILES = "attach_files", 12, Count, set(1, 64), "files attached to one message stay readable for the model",
        tools: ["attachments_list"], doc: "at most {n} files per message", src: [];
    ATTACH_TOTAL = "attach_total", 120, MiB, set(8, 1024), "attachments are held in memory for the chat",
        tools: ["attachments_list"], doc: "at most {n} MB of attachments per message", src: [];
    COPIES = "copies", 200, Count, set(2, 5000), "one call cannot flood the model with copies",
        tools: ["copy_elements", "array_elements"], doc: "at most {n} copies per call; split larger arrays", src: [];
    TERRAIN_POINTS = "terrain_points", 5000, Count, set(100, 100_000), "the surface is meshed in one step",
        tools: ["create_terrain"], doc: "at most {n} points per terrain", src: [];
    PREVIEW_ELEMENTS = "preview_elements", 5000, Count, set(500, 50_000), "the plan preview is drawn every frame",
        tools: [], doc: "", src: [];
    CRITIC_CYCLES = "critic_cycles", 3, Count, set(0, 10), "each review cycle costs a model step",
        tools: [], doc: "", src: [];
    VARIANTS = "variants", 4, Count, set(1, 12), "each variant is a full generator run",
        tools: ["compare_variants"], doc: "at most {n} variants per call", src: [];
    HTTP_READ_TIMEOUT = "http_read_timeout", 120, Seconds, set(10, 1800), "a stalled cloud model frees the chat",
        tools: [], doc: "", src: [];
    HTTP_READ_TIMEOUT_LOCAL = "http_read_timeout_local", 600, Seconds, set(30, 3600), "a local model may think long before it answers",
        tools: [], doc: "", src: [];
    MCP_READ_TIMEOUT = "mcp_read_timeout", 300, Seconds, set(10, 3600), "a stalled tool server frees the chat",
        tools: [], doc: "", src: [];

    // per turn and per plan: the project sets them, the mode chip lifts them
    STEPS = "steps", 24, Count, project(1, 100_000), "each step is one model request; a runaway turn stops here",
        tools: [], doc: "", src: [];
    TOOL_CALLS = "tool_calls", 64, Count, project(1, 1_000_000), "a runaway turn cannot call tools forever",
        tools: [], doc: "", src: [];
    TURN_TOKENS = "turn_tokens", 200_000, Count, project(1000, 1_000_000_000), "tokens of one turn cost money",
        tools: [], doc: "", src: [];
    WALL_S = "wall_s", 600, Seconds, project(10, 86_400), "a turn that never ends blocks the chat",
        tools: [], doc: "", src: [];
    RESULT_CHARS = "result_chars", 6000, Chars, project(500, 1_000_000), "a tool result rides in every later request",
        tools: [], doc: "", src: [];
    PLAN_CONFIRM = "plan_confirm", 500, Count, project(1, 10_000_000), "a plan touching more elements asks for Allow large",
        tools: [], doc: "", src: ["app/src/ai/validate.rs::LARGE_PLAN"];
    PLAN_DELETE = "plan_delete", 50, Count, project(1, 10_000_000), "a plan deleting more elements asks for Allow large",
        tools: [], doc: "", src: ["app/src/ai/validate.rs::LARGE_DELETE"];

    // guards: memory and loop safety
    PLAN_REFUSE = "plan_refuse", 150_000, Count, G, "a plan past the change list of the card cannot be shown or verified",
        tools: [], doc: "", src: ["app/src/ai/validate.rs::REFUSED_PLAN"];
    AUTO_CONTINUES = "auto_continues", 5, Count, G, "turns that go on by themselves after a limit stop, until the person writes again",
        tools: [], doc: "", src: ["app/src/ai/ui/mod.rs::MAX_AUTO_CONTINUES"];
    QUERY_PAGE = "query_page", 100, Count, G, "one reply stays small enough to read",
        tools: ["query_relations", "query_spatial"], doc: "returns at most {n} rows per call (limit); page on with offset", src: [];
    QUERY_DEFAULT = "query_default", 20, Count, G, "rows a query returns when no limit is given",
        tools: [], doc: "", src: ["app/src/ai/qrel.rs::DEFAULT_LIMIT", "app/src/ai/qspatial.rs::DEFAULT_LIMIT"];
    TOPO_PAGE = "topo_page", 200, Count, G, "one reply stays small enough to read",
        tools: ["query_topology"], doc: "returns at most {n} items per list (limit)", src: [];
    TOPO_DEFAULT = "topo_default", 50, Count, G, "items a topology list returns when no limit is given",
        tools: [], doc: "", src: ["app/src/ai/qrel.rs::TOPO_LIMIT"];
    RAY_DEFAULT = "ray_default", 100, Metres, G, "a ray without a length stops here",
        tools: [], doc: "", src: ["app/src/ai/qspatial.rs::DEFAULT_RAY_MAX"];
    TEXT_CLIP = "text_clip", 120, Chars, G, "model strings in a reply are summaries; a pasted instruction does not fit",
        tools: [], doc: "", src: ["app/src/ai/fmt.rs::TEXT_MAX"];
    ERROR_CLIP = "error_clip", 1000, Chars, G, "an error message stays readable",
        tools: [], doc: "", src: ["app/src/ai/fmt.rs::ERROR_MAX"];
    ATTACH_READ_PAGES = "attach_read_pages", 10, Count, G, "one reply stays small enough to read",
        tools: ["attachments_read"], doc: "at most {n} pages per call; ask for the next", src: ["app/src/ai/attach.rs::MAX_READ_PAGES"];
    ATTACH_READ_ROWS = "attach_read_rows", 500, Count, G, "one reply stays small enough to read",
        tools: ["attachments_read"], doc: "at most {n} sheet rows per call; ask for the next", src: ["app/src/ai/attach.rs::MAX_READ_ROWS"];
    CARD_CHARS = "card_chars", 400, Chars, G, "an element card is a summary",
        tools: [], doc: "", src: ["app/src/ai/card.rs::CARD_MAX"];
    CARD_PART = "card_part", 80, Chars, G, "one card field is a summary",
        tools: [], doc: "", src: ["app/src/ai/card.rs::PART_MAX"];
    CHECKPOINTS = "checkpoints", 32, Count, G, "undo points kept per chat",
        tools: [], doc: "", src: ["app/src/ai/checkpoints.rs::MAX_POINTS"];
    CHECKPOINT_LEDGER = "checkpoint_ledger", 64, Count, G, "ledger entries kept per chat",
        tools: [], doc: "", src: ["app/src/ai/checkpoints.rs::MAX_LEDGER"];
    CREATE_ITEMS = "create_items", 200, Count, G, "one plan item stays reviewable",
        tools: ["create_elements"], doc: "at most {n} items per call; split larger sets", src: ["ai/src/catalog.rs::MAX_CREATE_ITEMS"];
    TITLE_CHARS = "title_chars", 200, Chars, G, "a plan title is one line",
        tools: [], doc: "", src: ["ai/src/catalog.rs::MAX_TITLE_CHARS"];
    WHY_CHARS = "why_chars", 300, Chars, G, "a plan reason is one sentence",
        tools: [], doc: "", src: ["ai/src/catalog.rs::MAX_WHY_CHARS"];
    GEO_OPS = "geo_ops", 64, Count, G, "one geometry stays reviewable",
        tools: ["create_geometry"], doc: "at most {n} operations per call", src: ["ai/src/catalog.rs::MAX_GEO_OPS", "app/src/ai/geo_tools.rs::MAX_GEO_OPS"];
    SCRIPT_CHARS = "script_chars", 256 * 1024, Chars, G, "a script is parsed in one step",
        tools: ["run_script"], doc: "a script holds at most {n} characters", src: ["ai/src/catalog.rs::MAX_SCRIPT_CHARS"];
    SCRIPT_JOINED = "script_joined", 8 * 256 * 1024, Chars, G, "joined script parts are parsed in one step",
        tools: [], doc: "", src: ["app/src/ai/script_tools.rs::MAX_JOINED"];
    FAMILY_TEXT = "family_text", 200 * 1024, Chars, G, "a family definition is parsed in one step",
        tools: ["define_family"], doc: "a definition holds at most {n} characters", src: ["ai/src/catalog.rs::MAX_FAMILY_TEXT"];
    FAMILY_NAME = "family_name", 80, Chars, G, "a family name is a label",
        tools: ["define_family"], doc: "a family name holds at most {n} characters", src: ["ai/src/catalog.rs::MAX_FAMILY_NAME"];
    RETRIES = "retries", 2, Count, G, "a failing provider is not called forever",
        tools: [], doc: "", src: ["ai/src/convo.rs::MAX_RETRIES"];
    RETRY_WAIT = "retry_wait", 60, Seconds, G, "a retry never waits longer than this",
        tools: [], doc: "", src: ["ai/src/convo.rs::MAX_RETRY_WAIT_S"];
    STEP_BYTES = "step_bytes", 16 * 1024 * 1024, Bytes, G, "one model answer is held in memory",
        tools: [], doc: "", src: ["ai/src/convo.rs::MAX_STEP_BYTES"];
    KEPT_TEXT = "kept_text", 256 * 1024, Chars, G, "chat history kept in memory",
        tools: [], doc: "", src: ["ai/src/convo.rs::MAX_KEPT_TEXT_CHARS"];
    KEPT_ARGS = "kept_args", 1024, Bytes, G, "tool arguments kept in history",
        tools: [], doc: "", src: ["ai/src/convo.rs::MAX_KEPT_ARGS_BYTES"];
    TOOL_ID = "tool_id", 128, Bytes, G, "a tool call id is a short token",
        tools: [], doc: "", src: ["ai/src/convo.rs::MAX_TOOL_ID_BYTES"];
    SHOWN_ID = "shown_id", 64, Chars, G, "a name quoted in an error is cut",
        tools: [], doc: "", src: ["ai/src/registry.rs::MAX_SHOWN_ID", "ai/src/convo.rs::MAX_NAME_IN_ERROR_CHARS"];
    SHRINKS = "shrinks", 3, Count, G, "context shrinking stops after this many tries",
        tools: [], doc: "", src: ["ai/src/convo.rs::MAX_SHRINKS"];
    CONTINUES = "continues", 2, Count, G, "a reply cut off by the output limit is continued this many times",
        tools: [], doc: "", src: ["ai/src/convo.rs::MAX_CONTINUES"];
    OUTPUT_DEFAULT_CAP = "output_default_cap", 64_000, Count, G, "a default taken from a model list is capped",
        tools: [], doc: "", src: ["app/src/ai/ui/settings.rs::MAX_OUTPUT_DEFAULT"];
    TYPES_PER_CALL = "types_per_call", 24, Count, G, "one plan item per type stays reviewable",
        tools: [], doc: "", src: ["app/src/ai/create_more.rs::MAX_TYPES_PER_CALL"];
    CRITIC_LISTED = "critic_listed", 12, Count, G, "findings listed per review cycle; the rest are counted",
        tools: [], doc: "", src: ["app/src/ai/critic_hook.rs::MAX_LISTED"];
    REPAIR_SHOWN = "repair_shown", 5, Count, G, "findings shown per family repair; the rest are counted",
        tools: [], doc: "", src: ["ai/src/family_repair.rs::MAX_FINDINGS_SHOWN"];
    HTTP_CONNECT_TIMEOUT = "http_connect_timeout", 10, Seconds, G, "an unreachable address fails fast",
        tools: [], doc: "", src: ["ai/src/http.rs::CONNECT_TIMEOUT"];
    HTTP_WRITE_TIMEOUT = "http_write_timeout", 60, Seconds, G, "a stalled upload fails",
        tools: [], doc: "", src: ["ai/src/http.rs::WRITE_TIMEOUT"];
    LIST_MODELS_TIMEOUT = "list_models_timeout", 30, Seconds, G, "the model list is a quick question",
        tools: [], doc: "", src: ["ai/src/http.rs::LIST_MODELS_TIMEOUT"];
    ERROR_BODY = "error_body", 64 * 1024, Bytes, G, "an error page is read only this far",
        tools: [], doc: "", src: ["ai/src/http.rs::MAX_ERROR_BODY"];
    ERROR_CHARS = "error_chars", 2000, Chars, G, "a provider error stays readable",
        tools: [], doc: "", src: ["ai/src/http.rs::MAX_ERROR_CHARS"];
    MODELS_BODY = "models_body", 4 * 1024 * 1024, Bytes, G, "the model list is held in memory",
        tools: [], doc: "", src: ["ai/src/http.rs::MAX_MODELS_BODY"];
    CLIENT_NAME = "client_name", 64, Chars, G, "a client name is a label",
        tools: [], doc: "", src: ["ai/src/mcp/rpc.rs::CLIENT_NAME_MAX"];
    TOOL_ARGS_BYTES = "tool_args_bytes", 1024 * 1024, Bytes, G, "tool arguments are parsed in memory",
        tools: [], doc: "", src: ["ai/src/provider.rs::MAX_TOOL_ARGS_BYTES", "ai/src/proto_text_tools.rs::MAX_HOLD_BYTES"];
    FENCE_RETRIES = "fence_retries", 64, Count, G, "a broken tool block is not parsed forever",
        tools: [], doc: "", src: ["ai/src/proto_text_tools.rs::MAX_FENCE_RETRIES"];
    CALLS_PER_STEP = "calls_per_step", 256, Count, G, "a runaway answer cannot queue endless calls",
        tools: [], doc: "", src: ["ai/src/provider.rs::MAX_CALLS_PER_STEP"];
    MCP_WRITE_TIMEOUT = "mcp_write_timeout", 30, Seconds, G, "a stalled agent connection is dropped",
        tools: [], doc: "", src: ["ai/src/mcp/server.rs::WRITE_TIMEOUT"];
    MCP_REFUSE_TIMEOUT = "mcp_refuse_timeout", 200, Millis, G, "a refused connection is not waited on",
        tools: [], doc: "", src: ["ai/src/mcp/server.rs::REFUSE_WRITE_TIMEOUT"];
    MCP_CLIENTS = "mcp_clients", 64, Count, G, "open agent connections",
        tools: [], doc: "", src: ["ai/src/mcp/server.rs::MAX_CLIENTS"];
    MCP_SESSIONS = "mcp_sessions", 32, Count, G, "agent sessions kept",
        tools: [], doc: "", src: ["ai/src/mcp/server.rs::MAX_SESSIONS"];
    MCP_RESOURCE_READS = "mcp_resource_reads", 256, Count, G, "reads kept while the editor is busy; the oldest go first",
        tools: [], doc: "", src: ["ai/src/mcp/server.rs::MAX_RESOURCE_READS"];
    MCP_LINE = "mcp_line", 4 << 20, Bytes, G, "one tool server message is held in memory",
        tools: [], doc: "", src: ["ai/src/mcp/stdio.rs::MAX_LINE"];
    MCP_REPLY = "mcp_reply", 64 << 20, Bytes, G, "one tool server reply is held in memory",
        tools: [], doc: "", src: ["ai/src/mcp/stdio.rs::MAX_REPLY"];
    MCP_CONNECT_TIMEOUT = "mcp_connect_timeout", 5, Seconds, G, "a tool server that does not start fails fast",
        tools: [], doc: "", src: ["ai/src/mcp/stdio.rs::CONNECT_TIMEOUT"];
    MCP_TEST_TIMEOUT = "mcp_test_timeout", 300, Seconds, G, "the test client gives up on a silent server",
        tools: [], doc: "", src: ["ai/src/mcp/test_client.rs::TIMEOUT"];
    SECRETS_KNOWN = "secrets_known", 64, Count, G, "secret values masked in logs",
        tools: [], doc: "", src: ["ai/src/secret.rs::MAX_KNOWN"];
    SKILL_BYTES = "skill_bytes", 6 * 1024, Bytes, G, "a skill note rides in every prompt",
        tools: [], doc: "", src: ["ai/src/skills.rs::MAX_BYTES"];
    SSE_EVENT = "sse_event", 8 * 1024 * 1024, Bytes, G, "one streamed event is held in memory",
        tools: [], doc: "", src: ["ai/src/sse.rs::MAX_EVENT_BYTES"];
    SCHEMA_STR = "schema_str", 256 * 1024, Chars, G, "one argument string is checked in memory",
        tools: [], doc: "", src: ["ai/src/schema.rs::MAX_STR_CHARS"];
    SCHEMA_ITEMS = "schema_items", 10_000, Count, G, "one argument list is checked in memory",
        tools: [], doc: "", src: ["ai/src/schema.rs::MAX_ITEMS"];
    SCHEMA_KEYS = "schema_keys", 1000, Count, G, "one argument map is checked in memory",
        tools: [], doc: "", src: ["ai/src/schema.rs::MAX_MAP_KEYS"];
    SCHEMA_FIELD_SHOWN = "schema_field_shown", 64, Chars, G, "a field name quoted in an error is cut",
        tools: [], doc: "", src: ["ai/src/schema.rs::MAX_FIELD_NAME_SHOWN"];
    FIXTURES = "fixtures", 200, Count, G, "one plan item stays reviewable",
        tools: ["place_fixtures"], doc: "at most {n} fixtures per call", src: ["app/src/ai/mep_tools.rs::MAX_FIXTURES"];
    DIFF_CHARS = "diff_chars", 200_000, Chars, G, "a plan diff is held in memory",
        tools: [], doc: "", src: ["app/src/ai/options_gen.rs::DIFF_LIMIT", "app/src/ai/session.rs::DIFF_LIMIT"];
    METRIC_KEYS = "metric_keys", 8, Count, G, "an option compares on a few metrics",
        tools: [], doc: "", src: ["app/src/ai/options_keep.rs::MAX_METRIC_KEYS"];
    REF_RANGE = "ref_range", 10_000, Count, G, "one reference range stays countable",
        tools: [], doc: "", src: ["app/src/ai/plan.rs::MAX_RANGE"];
    POINT_TARGETS = "point_targets", 50, Count, G, "one pointing question stays readable",
        tools: [], doc: "", src: ["app/src/ai/point.rs::MAX_TARGETS"];
    POINT_PROPS = "point_props", 30, Count, G, "one pointed element is a summary",
        tools: [], doc: "", src: ["app/src/ai/point.rs::MAX_PROPS"];
    HIGHLIGHT = "highlight", 200, Count, G, "the view stays readable",
        tools: ["view_snapshot"], doc: "highlight at most {n}", src: ["app/src/ai/pose.rs::MAX_HIGHLIGHT"];
    LABELS = "labels", 100, Count, G, "labels stay readable in one picture",
        tools: ["view_snapshot"], doc: "label at most {n}", src: ["app/src/ai/pose.rs::MAX_LABELS"];
    POSE_GAP = "pose_gap", 50, Metres, G, "the camera stays near the model",
        tools: [], doc: "", src: ["app/src/ai/pose.rs::MAX_GAP"];
    HATCH_FRAME = "hatch_frame", 20_000, Count, G, "hatch strokes drawn per preview frame",
        tools: [], doc: "", src: ["app/src/ai/preview.rs::HATCH_FRAME_MAX"];
    PRIMED = "primed", 200, Count, G, "generator names primed into the plan",
        tools: [], doc: "", src: ["app/src/ai/project_plan.rs::PRIMED_MAX"];
    RECEIPT_BYTES = "receipt_bytes", 8 * 1024, Bytes, G, "a receipt rides in the next prompt; counts stay exact, lists give way",
        tools: [], doc: "", src: ["app/src/ai/receipt.rs::MAX_BYTES"];
    RECEIPT_QTY = "receipt_qty", 24, Count, G, "quantities listed per receipt; the rest are counted",
        tools: [], doc: "", src: ["app/src/ai/receipt.rs::MAX_QTY"];
    RECEIPT_FINDINGS = "receipt_findings", 20, Count, G, "findings listed per receipt; the rest are counted",
        tools: [], doc: "", src: ["app/src/ai/receipt.rs::MAX_FINDINGS"];
    REFERENCE_SIDE = "reference_side", 768, Pixels, G, "a reference image is scaled down before it is sent",
        tools: ["get_reference"], doc: "images are scaled to at most {n} px a side", src: ["app/src/ai/reference.rs::MAX_SIDE"];
    REFERENCE_BYTES = "reference_bytes", 20 * 1024 * 1024, Bytes, G, "a reference file is read into memory",
        tools: ["get_reference"], doc: "reads files up to {n} bytes", src: ["app/src/ai/reference.rs::MAX_BYTES"];
    RENDER_SIDE = "render_side", 1280, Pixels, G, "a picture costs tokens by its size",
        tools: ["render_plan", "render_3d"], doc: "at most {n} px a side", src: ["app/src/ai/render_req.rs::MAX_SIDE"];
    RENDER_PIXELS = "render_pixels", 1_000_000, Pixels, G, "a picture costs tokens by its size",
        tools: ["render_plan", "render_3d"], doc: "at most {n} pixels", src: ["app/src/ai/render_req.rs::MAX_PIXELS"];
    RENDER_SCALE = "render_scale", 2000, Count, G, "a drawing scale beyond 1:{n} shows nothing",
        tools: ["render_plan"], doc: "scale at most 1:{n}", src: ["app/src/ai/render_req.rs::MAX_SCALE"];
    RENDER_LEGEND = "render_legend", 60, Count, G, "a legend stays readable",
        tools: ["render_plan", "render_3d"], doc: "at most {n} legend rows", src: ["app/src/ai/render_req.rs::MAX_LEGEND"];
    REPLAN_TRIES = "replan_tries", 2, Count, G, "a refused plan is redrafted only so often",
        tools: [], doc: "", src: ["app/src/ai/replan.rs::MAX_TRIES"];
    SCHED_COLS = "sched_cols", 24, Count, G, "a schedule stays readable",
        tools: ["create_schedule"], doc: "at most {n} columns", src: ["app/src/ai/sched_tool.rs::MAX_COLS", "app/src/ai/tools_read.rs::MAX_COLS"];
    SCHED_GROUPS = "sched_groups", 3, Count, G, "grouping deeper than this is unreadable",
        tools: ["create_schedule"], doc: "at most {n} grouping levels", src: ["app/src/ai/sched_tool.rs::MAX_GROUPS"];
    SCHED_HEADS = "sched_heads", 40, Chars, G, "a column head is a label",
        tools: [], doc: "", src: ["app/src/ai/sched_tool.rs::MAX_HEADS"];
    SETS = "sets", 64, Count, G, "element sets kept per chat; the oldest go first",
        tools: [], doc: "", src: ["app/src/ai/sets.rs::MAX_SETS"];
    SET_KEYS = "set_keys", 4_000_000, Count, G, "element ids kept across all sets",
        tools: [], doc: "", src: ["app/src/ai/sets.rs::MAX_KEYS"];
    SNAPSHOT_SIDE = "snapshot_side", 1280, Pixels, G, "a picture costs tokens by its size",
        tools: ["view_snapshot"], doc: "at most {n} px a side", src: ["app/src/ai/snapshot.rs::MAX_SIDE"];
    SNAPSHOT_PNG = "snapshot_png", 1_500_000, Bytes, G, "a picture rides in the prompt",
        tools: [], doc: "", src: ["app/src/ai/snapshot.rs::MAX_PNG"];
    FINDINGS = "findings", 200, Count, G, "findings listed per reply; the rest are counted",
        tools: ["run_checks"], doc: "lists at most {n} findings; the rest are counted", src: ["app/src/ai/tools_read.rs::MAX_FINDINGS"];
    REQ_LIST = "req_list", 20, Count, G, "requirements listed per reply",
        tools: [], doc: "", src: ["app/src/ai/tools_read.rs::REQ_LIMIT"];
    MCP_OPS = "mcp_ops", 5000, Count, G, "agent operations kept per session",
        tools: [], doc: "", src: ["app/src/ai/session/mcp_ops.rs::MAX_OPS"];
    REFUSED_PLAN = "refused_plan", 199_999, Chars, G, "a refused plan is quoted back only this far",
        tools: [], doc: "", src: ["app/src/ai/validate.rs::REFUSED_PLAN_MAX"];
    RULE_FINDINGS = "rule_findings", 200, Count, G, "rule findings listed per check",
        tools: [], doc: "", src: ["app/src/ai/validate.rs::MAX_RULE_FINDINGS"];
    LEGEND_ROWS = "legend_rows", 24, Count, G, "a color legend stays readable; rarer values fold into one row",
        tools: ["color_by"], doc: "at most {n} legend rows; rarer values fold into one", src: ["app/src/ai/view_tools.rs::MAX_ROWS"];
    UI_FINDINGS = "ui_findings", 500, Count, G, "findings drawn in the checks list",
        tools: [], doc: "", src: ["app/src/ai/ui/checks.rs::FINDINGS_MAX"];
    COMPARE_TILES = "compare_tiles", 4, Count, G, "variants side by side on one screen",
        tools: [], doc: "", src: ["app/src/ai/ui/compare_logic.rs::TILES_MAX"];
    UI_DETAIL = "ui_detail", 300, Chars, G, "a step detail line on screen is a summary",
        tools: [], doc: "", src: ["app/src/ai/ui/logic.rs::DETAIL_MAX", "app/src/ai/ui/steps.rs::DETAIL_MAX"];
    MD_DEPTH = "md_depth", 8, Count, G, "nested lists deeper than this draw flat",
        tools: [], doc: "", src: ["app/src/ai/ui/md.rs::MAX_DEPTH"];
    NORM_ROWS = "norm_rows", 200, Count, G, "norm rows drawn in the panel",
        tools: [], doc: "", src: ["app/src/ai/ui/norms.rs::ROWS_MAX"];
    POINT_MARKS = "point_marks", 100, Count, G, "marks drawn over the view",
        tools: [], doc: "", src: ["app/src/ai/ui/point_marks.rs::MAX_MARKS"];
    RECEIPT_IDS = "receipt_ids", 500, Count, G, "ids kept per receipt card",
        tools: [], doc: "", src: ["app/src/ai/ui/receipt.rs::IDS_CAP"];
    REQ_MANUAL = "req_manual", 50, Count, G, "manual requirements shown",
        tools: [], doc: "", src: ["app/src/ai/ui/requirements.rs::MANUAL_MAX"];
    REQ_ERRORS = "req_errors", 20, Count, G, "requirement errors shown",
        tools: [], doc: "", src: ["app/src/ai/ui/requirements.rs::ERRORS_MAX"];
    RETRY_TRIES = "retry_tries", 3, Count, G, "a failed step is retried only so often",
        tools: [], doc: "", src: ["app/src/ai/ui/retry.rs::MAX_TRIES"];
    RETRY_WAIT_UI = "retry_wait_ui", 120, Seconds, G, "the longest wait before a retry",
        tools: [], doc: "", src: ["app/src/ai/ui/retry.rs::WAIT_MAX_S"];
    STEP_NAME = "step_name", 48, Chars, G, "a step name on screen is a label",
        tools: [], doc: "", src: ["app/src/ai/ui/steps.rs::NAME_MAX"];
}

/// How far one turn may go. `Normal` is the registry's values; `Large` raises them for a big
/// model; `Unlimited` lifts steps, calls, tokens, time and plan size. A money cap and the
/// mass-delete confirmation stay in every mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reach {
    #[default]
    Normal,
    Large,
    Unlimited,
}

impl Reach {
    pub const ALL: [Reach; 3] = [Reach::Normal, Reach::Large, Reach::Unlimited];

    pub const fn key(self) -> &'static str {
        match self {
            Reach::Normal => "normal",
            Reach::Large => "large",
            Reach::Unlimited => "unlimited",
        }
    }

    pub fn parse(s: &str) -> Option<Reach> {
        Reach::ALL.into_iter().find(|r| r.key() == s.trim().to_ascii_lowercase())
    }

    /// The value of a turn or plan limit in this mode; `u64::MAX` is no limit.
    pub fn value(self, l: &Limit) -> u64 {
        match (self, l.key) {
            (Reach::Normal, _) => l.value,
            (Reach::Unlimited, "plan_delete") => l.value,
            (Reach::Unlimited, "result_chars") => 12_000,
            (Reach::Unlimited, _) => u64::MAX,
            (Reach::Large, "steps") => 120,
            (Reach::Large, "tool_calls") => 400,
            (Reach::Large, "turn_tokens") => 2_000_000,
            (Reach::Large, "wall_s") => 3600,
            (Reach::Large, "result_chars") => 12_000,
            (Reach::Large, "plan_confirm") => 5000,
            (Reach::Large, _) => l.value,
        }
    }
}

static DEFAULT_REACH: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

static PROFILE_REACH: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The mode of a project that made no choice: the active connection's own mode when it has
/// one, else the person's own last pick (their settings).
pub fn default_reach() -> Reach {
    match PROFILE_REACH.load(std::sync::atomic::Ordering::Relaxed) {
        0 => Reach::ALL[usize::from(DEFAULT_REACH.load(std::sync::atomic::Ordering::Relaxed)).min(2)],
        n => Reach::ALL[usize::from(n - 1).min(2)],
    }
}

/// The mode saved with the connection in use; `None` leaves the person's settings to decide.
pub fn set_profile_reach(r: Option<Reach>) {
    let v = r.and_then(|r| Reach::ALL.iter().position(|x| *x == r)).map_or(0, |i| i as u8 + 1);
    PROFILE_REACH.store(v, std::sync::atomic::Ordering::Relaxed);
}

pub fn set_default_reach(r: Reach) {
    let i = Reach::ALL.iter().position(|x| *x == r).unwrap_or(0);
    DEFAULT_REACH.store(i as u8, std::sync::atomic::Ordering::Relaxed);
}

/// `DDC_AI_UNLIMITED=1` (or true, yes, on): the command line's and scripts' way to the
/// unlimited mode.
pub fn env_unlimited() -> bool {
    std::env::var("DDC_AI_UNLIMITED")
        .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

/// Settings tier only, in declaration order: the rows of Settings > AI.
pub fn settings() -> impl Iterator<Item = &'static Limit> {
    ALL.iter().copied().filter(|l| l.is_setting())
}

pub fn find(key: &str) -> Option<&'static Limit> {
    ALL.iter().copied().find(|l| l.key == key)
}

/// Replaces every override; unknown keys and guards are ignored.
pub fn set_overrides<'a>(pairs: impl IntoIterator<Item = (&'a str, u64)>) {
    let mut o = OVERRIDES.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    o.clear();
    for (k, v) in pairs {
        if let Some(l) = find(k).filter(|l| l.is_setting()) {
            o.insert(l.key.to_string(), v);
        }
    }
}

/// The limits sentence a tool description ends with; empty when the tool has none.
pub fn describe(tool: &str) -> String {
    let parts: Vec<String> = ALL
        .iter()
        .filter(|l| l.tools.contains(&tool) && !l.doc.is_empty())
        .map(|l| l.doc.replace("{n}", &l.get().to_string()))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" Limits: {}.", parts.join("; "))
    }
}

/// `text` with its tool's limits appended.
pub fn with_limits(tool: &str, text: &str) -> String {
    let d = describe(tool);
    if d.is_empty() {
        text.to_string()
    } else {
        format!("{}{d}", text.trim_end())
    }
}

/// `limit` and `offset` of a paged call: the asked limit kept to `max`, and a note when it was cut.
pub fn page_args(a: &J, default: usize, max: &Limit) -> (usize, usize, Option<String>) {
    let cap = max.get_us();
    let asked = a["limit"].as_u64();
    let limit = asked.map_or(default, |n| (n as usize).clamp(1, cap));
    let offset = a["offset"].as_u64().unwrap_or(0) as usize;
    let note = asked.filter(|n| *n as usize > cap).map(|n| {
        format!("limit {n} was lowered to {cap} (limit `{}`: {}); page on with offset", max.key, max.reason)
    });
    (limit, offset, note)
}

/// The marker of a cut list: `{shown, total, next:{offset}, note}`; `next` is absent on the last page.
pub fn truncated(offset: usize, shown: usize, total: usize) -> J {
    let next = offset + shown;
    if next >= total {
        return json!({"shown": shown, "total": total, "note": format!("shown {shown} of {total}")});
    }
    json!({"shown": shown, "total": total, "next": {"offset": next},
        "note": format!("truncated: shown {shown} of {total}; next: offset={next}")})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `const NAME: T` lines naming a limit, as `crate/path.rs::NAME`.
    fn scan(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<(String, String)>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                scan(&p, root, out);
                continue;
            }
            if p.extension().is_none_or(|x| x != "rs") {
                continue;
            }
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            let src = std::fs::read_to_string(&p).unwrap();
            for line in src.lines() {
                let Some(i) = line.find("const ") else { continue };
                let rest = &line[i + 6..];
                let name: String = rest.chars().take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_').collect();
                if name.is_empty() || !rest[name.len()..].trim_start().starts_with(':') {
                    continue;
                }
                if !name.ends_with("_KEY") && ["MAX", "LIMIT", "CAP", "TIMEOUT", "LARGE", "REFUSED"].iter().any(|w| name.contains(w)) {
                    out.push((format!("{rel}::{name}"), line.to_string()));
                }
            }
        }
    }

    /// Names that match the scan but are not limits.
    const NOT_LIMITS: &[&str] = &[
        "ai/src/mcp/rpc.rs::KEY_CAPS",         // a protocol key
        "ai/src/registry.rs::LIMIT",           // words that mark a limit refusal
        "app/src/ai/ui/chat.rs::CONNECT_TOP_MAX", // layout spacing
        "app/src/ai/ui/modes_view.rs::CARD_MAX",  // layout width
        "app/src/ai/apply_verify_tests.rs::LIMIT", // test fixture
        "ai/src/limits.rs::NOT_LIMITS",
    ];

    #[test]
    fn registry_covers_every_limit_constant_and_each_reads_from_it() {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let mut found = vec![];
        scan(&crates.join("ai/src"), crates, &mut found);
        scan(&crates.join("app/src/ai"), crates, &mut found);
        assert!(found.len() > 80, "scan found {}", found.len());
        let mut missing = vec![];
        for (id, line) in &found {
            if NOT_LIMITS.contains(&id.as_str()) {
                continue;
            }
            let Some(l) = ALL.iter().find(|l| l.src.contains(&id.as_str())) else {
                missing.push(format!("{id}: not in limits::ALL"));
                continue;
            };
            let id_ref = format!("{}::{}", "limits", const_name(l));
            if !line.contains(&id_ref) {
                missing.push(format!("{id}: should read {id_ref}"));
            }
        }
        assert!(missing.is_empty(), "{}", missing.join("\n"));
        for l in ALL {
            for s in l.src {
                assert!(found.iter().any(|(id, _)| id == s), "{} names {s}, which is gone", l.key);
            }
        }
    }

    fn const_name(l: &Limit) -> String {
        l.key.to_uppercase()
    }

    #[test]
    fn keys_are_unique_and_documented() {
        let mut seen = std::collections::BTreeSet::new();
        for l in ALL {
            assert!(seen.insert(l.key), "{} twice", l.key);
            assert!(!l.reason.is_empty() && !l.reason.contains('\n'));
            assert_eq!(l.tools.is_empty(), l.doc.is_empty(), "{}: tools and doc go together", l.key);
            if let Tier::Setting { min, max } | Tier::Project { min, max } = l.tier {
                assert!(min <= l.value && l.value <= max, "{}", l.key);
            }
        }
    }

    #[test]
    fn every_tool_named_exists() {
        for l in ALL {
            for t in l.tools {
                assert!(crate::catalog::find(t).is_some(), "{}: no tool {t}", l.key);
            }
        }
    }

    #[test]
    fn a_tool_description_states_its_limit() {
        assert!(describe("query_relations").contains("at most 100 rows"));
        assert!(describe("query_relations").contains("offset"));
        assert!(describe("get_context").is_empty());
        let spec = crate::catalog::find("query_relations").unwrap();
        assert!(crate::toolset::wire(spec).description.contains("at most 100 rows"));
        let all = crate::catalog::schemas_at(crate::catalog::Tier::T3Auto, true);
        let q = all.iter().find(|s| s.name == "query_spatial").unwrap();
        assert!(q.description.contains("offset"), "{}", q.description);
    }

    #[test]
    fn an_override_applies_within_bounds_and_guards_stay() {
        set_overrides([("copies", 500), ("query_page", 5), ("nope", 1)]);
        assert_eq!(COPIES.get(), 500);
        assert!(describe("copy_elements").contains("500"));
        assert_eq!(QUERY_PAGE.get(), 100);
        set_overrides([("copies", 10_000_000)]);
        assert_eq!(COPIES.get(), 5000);
        set_overrides([]);
        assert_eq!(COPIES.get(), 200);
        assert!(COPIES.refusal("300 copies").contains("Settings > AI"));
    }

    #[test]
    fn a_connections_mode_outranks_the_settings_and_a_profile_file_keeps_it() {
        set_default_reach(Reach::Large);
        assert_eq!(default_reach(), Reach::Large);
        set_profile_reach(Some(Reach::Unlimited));
        assert_eq!(default_reach(), Reach::Unlimited);
        set_profile_reach(None);
        assert_eq!(default_reach(), Reach::Large);
        set_default_reach(Reach::Normal);
        let mut p = crate::profile::blank("p".into(), "p".into());
        assert!(!serde_json::to_string(&p).unwrap().contains("reach"));
        p.reach = Some(Reach::Large);
        let back: crate::profile::Profile = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back.reach, Some(Reach::Large));
    }

    #[test]
    fn reach_lifts_turn_and_plan_limits_but_keeps_mass_delete() {
        assert_eq!(Reach::parse(" Unlimited "), Some(Reach::Unlimited));
        assert_eq!(Reach::parse("x"), None);
        for l in ALL.iter().filter(|l| matches!(l.tier, Tier::Project { .. })) {
            assert_eq!(Reach::Normal.value(l), l.value);
            let (lg, un) = (Reach::Large.value(l), Reach::Unlimited.value(l));
            assert!(lg >= l.value && un >= lg, "{}", l.key);
            assert_eq!(un == u64::MAX, !["plan_delete", "result_chars"].contains(&l.key), "{}", l.key);
        }
    }

    #[test]
    fn truncated_names_shown_total_and_next() {
        let t = truncated(0, 100, 250);
        assert_eq!(t["next"]["offset"], 100);
        assert!(t["note"].as_str().unwrap().contains("shown 100 of 250"));
        assert!(truncated(200, 50, 250).get("next").is_none());
    }
}
