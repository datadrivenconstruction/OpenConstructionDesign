//! Joins the MCP server to the live editor: the settings step that turns it on, the indicator with
//! one-click revoke and policy switch, and the hand-over of calls on the UI thread.

use super::agent_view;
use super::fmt;
use super::ui::kit::Button;
use super::session::{
    self, live_policy, with_session, SessionCfg, UiSession, ViewCtx, ACTOR_PREFIX, POLICY_KEY,
};
use crate::editor::Editor;
use crate::shell::EditorApp;
use ddc_ai::mcp::{self, ClientSeen, McpCall, McpConfig, McpHandle, McpLimits, McpPolicy};
use ddc_ai::{
    ChatRequest, Flow, Limits, Provider, ProviderError, Secret, SecretStore, StreamEvent,
};
use ddc_model::AuditEntry;
use ddc_ui::notify::Level;
use serde_json::{json, Value as J};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use web_time::{Duration, Instant};

pub const DEFAULT_PORT: u16 = 47621;
const SETTINGS_FILE: &str = "mcp.json";
const INERT_PREFIX: &str = "mcp-";
const NO_SESSION: &str = "mcp";
const FEED_ROWS: i64 = 200;
/// Audit event of a guide read; `endpoint` holds the address.
pub const RESOURCE_READ: &str = "mcp_resource_read";
const FEED_REFRESH: Duration = Duration::from_secs(1);
const WORK_FRAME: Duration = Duration::from_millis(30);
const INDICATOR_FRAME: Duration = Duration::from_secs(1);

thread_local! {
    // per thread: the worker replaces the model of its scratch copy, and that is no project change
    static MODEL_EPOCH: Cell<u64> = const { Cell::new(0) };
    static UI: RefCell<BridgeUi> = RefCell::new(BridgeUi::default());
    // the model epoch in which the person said yes to applying without asking; memory only, so an
    // open never writes the file (read-only files included) and no stored value outlives a session
    static AUTO_ARMED: Cell<Option<u64>> = const { Cell::new(None) };
}

/// The stored policy as it holds now: applying without asking counts only when it was chosen in
/// this session for this model.
pub fn session_policy(stored: McpPolicy) -> McpPolicy {
    match stored {
        McpPolicy::AutoApply if AUTO_ARMED.with(Cell::get) == Some(model_epoch()) => {
            McpPolicy::AutoApply
        }
        _ => McpPolicy::Confirm,
    }
}

/// The live editor got another model: plans of the old one must not reach it.
pub fn on_model_replaced() {
    MODEL_EPOCH.with(|e| e.set(e.get() + 1));
}

pub fn model_epoch() -> u64 {
    MODEL_EPOCH.with(Cell::get)
}

/// Off until a person ticks the box: no listener and no token before that.
#[derive(Clone, Debug, PartialEq)]
pub struct McpSettings {
    pub enabled: bool,
    pub port: u16,
}

impl Default for McpSettings {
    fn default() -> Self {
        McpSettings {
            enabled: false,
            port: DEFAULT_PORT,
        }
    }
}

impl McpSettings {
    /// An unreadable file means off: a damaged setting must not open a door.
    pub fn load_from(path: &Path) -> Self {
        let parsed: Option<J> = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        let Some(v) = parsed else {
            return Self::default();
        };
        let port = v["port"]
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p >= 1024)
            .unwrap_or(DEFAULT_PORT);
        McpSettings {
            enabled: v["enabled"].as_bool().unwrap_or(false),
            port,
        }
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            json!({"enabled": self.enabled, "port": self.port}).to_string(),
        )?;
        std::fs::rename(&tmp, path)
    }
}

fn audit(ed: &Editor, session: &str, actor: &str, event: &str, plan: Option<String>, status: &str) {
    let e = AuditEntry {
        session: session.to_string(),
        actor: actor.to_string(),
        event: event.to_string(),
        plan,
        status: status.to_string(),
        ..AuditEntry::default()
    };
    crate::ai::session::audit_write(&ed.model, &e);
}

/// The person's choice, written to the live file: the next apply reads it from there.
pub fn set_policy(ed: &Editor, session: &str, policy: McpPolicy) -> Result<(), String> {
    ed.model
        .set_ai_policy(POLICY_KEY, policy.setting())
        .map_err(|e| e.to_string())?;
    let armed = (policy == McpPolicy::AutoApply).then(model_epoch);
    AUTO_ARMED.with(|a| a.set(armed));
    audit(
        ed,
        session,
        "user",
        "mcp_policy",
        Some(policy.setting().to_string()),
        "ok",
    );
    Ok(())
}

/// Applying without asking lasts one session: a start or another model asks again, whatever the
/// file stored. True when it was on and is now off. Memory only: the file is not written.
pub fn session_reset(ed: &Editor, _session: &str) -> Result<bool, String> {
    if live_policy(ed) != McpPolicy::AutoApply {
        return Ok(false);
    }
    AUTO_ARMED.with(|a| a.set(None));
    Ok(true)
}

/// A choice that only a yes in a dialog carries out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    AutoApply,
    NewToken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyStep {
    Keep,
    Set(McpPolicy),
    Ask,
}

/// Asking again is immediate; applying without asking is never set here, only asked about.
pub fn policy_step(current: McpPolicy, want: McpPolicy) -> PolicyStep {
    match (current, want) {
        // an external agent's writes always wait in its draft: there is nothing to switch on
        (McpPolicy::AutoApply, McpPolicy::Confirm) => PolicyStep::Set(McpPolicy::Confirm),
        _ => PolicyStep::Keep,
    }
}

/// The status line stays up while an agent's draft waits, even with no agent connected.
pub fn bar_visible(clients: bool, waiting: bool, drafts: bool) -> bool {
    clients || waiting || drafts
}

/// Title, body and confirm button of each question.
pub fn ask_keys(a: Ask) -> (&'static str, &'static str, &'static str) {
    match a {
        Ask::AutoApply => ("mcp.ask.auto.title", "mcp.ask.auto.body", "mcp.ask.auto.yes"),
        Ask::NewToken => ("mcp.ask.token.title", "mcp.ask.token.body", "mcp.ask.token.yes"),
    }
}

/// Ports are identifiers: whole numbers, no group separators.
const PORT_FORMAT: ddc_ui::widgets::NumberFormat = ddc_ui::widgets::NumberFormat {
    decimal: '.',
    group: None,
};

/// Five digits and the field frame: three controls wide, not the width of the dialog.
const PORT_W: f32 = ddc_ui::sem::control::HIT_MIN * 3.0;

pub fn port_of(v: f64) -> Option<u16> {
    let v = v.round();
    (v.is_finite() && (1024.0..=65535.0).contains(&v)).then_some(v as u16)
}

/// The running server: where it listens and how many agents are connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Live {
    pub port: u16,
    pub clients: usize,
}

/// Text and tone of the status chip. `failed` is a start that reported an error.
pub(crate) fn status_text(lang: ddc_ui::Lang, enabled: bool, failed: bool, live: Option<Live>) -> (String, ddc_ui::widgets::Tone) {
    use ddc_ui::widgets::Tone;
    match (live, enabled, failed) {
        (Some(l), _, _) => {
            let addr = format!("127.0.0.1:{}", l.port);
            (crate::tr_in(lang, "mcp.set.state_on", &[("addr", addr.as_str())]), Tone::Success)
        }
        (None, true, true) => (crate::tr_in(lang, "mcp.set.state_failed", &[]), Tone::Danger),
        (None, true, false) => (crate::tr_in(lang, "mcp.set.state_starting", &[]), Tone::Warning),
        (None, false, _) => (crate::tr_in(lang, "mcp.set.state_off", &[]), Tone::Neutral),
    }
}

/// Width of the caption column: every control of the form starts on one x.
const LABEL_W: f32 = ddc_ui::sem::control::DROPDOWN_W * 0.75;

/// One form line: muted caption in the shared column, then the control. The control gets the
/// caption's id to name itself by.
fn form_row<R>(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    caption: &str,
    add: impl FnOnce(&mut egui::Ui, egui::Id) -> R,
) -> R {
    use ddc_ui::theme::Role;
    ui.horizontal(|ui| {
        let lid = ui
            .allocate_ui_with_layout(
                egui::vec2(LABEL_W, th.d().control),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_width(LABEL_W);
                    let text = egui::RichText::new(caption)
                        .font(th.font(Role::Label))
                        .color(th.sem().text_secondary);
                    ui.add(egui::Label::new(text).truncate()).id
                },
            )
            .inner;
        add(ui, lid)
    })
    .inner
}

/// Muted explanation under a control, in the control column and apart from the caption.
fn row_hint(ui: &mut egui::Ui, th: &ddc_ui::Theme, text: &str) {
    use ddc_ui::theme::Role;
    ui.horizontal(|ui| {
        ui.add_space(LABEL_W);
        let t = egui::RichText::new(text)
            .font(th.font(Role::Caption))
            .color(th.sem().text_secondary);
        ui.add(egui::Label::new(t).wrap());
    });
}

/// Status chip, with the number of connected agents beside it while it listens.
fn status_row(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    lang: ddc_ui::Lang,
    enabled: bool,
    failed: bool,
    live: Option<Live>,
) {
    let (text, tone) = status_text(lang, enabled, failed, live);
    form_row(ui, th, &crate::tr_in(lang, "mcp.set.status", &[]), |ui, _| {
        ddc_ui::widgets::badge(ui, th, &text, tone);
        if let Some(l) = live {
            let n = l.clients.to_string();
            let t = crate::tr_in(lang, "mcp.set.clients", &[("n", n.as_str())]);
            ui.label(egui::RichText::new(t).color(th.c("text.secondary")));
        }
    });
}

/// Compact port field with its range; the new port when it changed. The rect is the field's.
pub(crate) fn port_row(ui: &mut egui::Ui, th: &ddc_ui::Theme, lang: ddc_ui::Lang, port: u16, locked: bool) -> (Option<u16>, egui::Rect) {
    let mut rect = egui::Rect::NOTHING;
    let mut changed = None;
    ui.add_enabled_ui(!locked, |ui| {
        let text = crate::tr_in(lang, "mcp.set.port", &[]);
        form_row(ui, th, &text, |ui, lid| {
            let mut v = f64::from(port);
            let out = ddc_ui::widgets::NumberField::new("ddc.mcp.port", &mut v, PORT_FORMAT)
                .labelled_by(lid, &text)
                .decimals(0)
                .range(1024.0, 65535.0)
                .width(PORT_W)
                .show(ui, th);
            rect = out.response.rect;
            changed = port_of(v).filter(|p| *p != port);
        });
        row_hint(ui, th, &crate::tr_in(lang, "mcp.set.port_range", &[]));
    });
    (changed, rect)
}

/// What the token rows asked for this frame.
#[derive(Default)]
pub(crate) struct TokenOut {
    pub hide: bool,
    pub renew: bool,
    pub copied: bool,
    pub copy_cfg: bool,
}

/// Token and config rows. Off, only the way to get a token is said; the buttons appear when on.
pub(crate) fn token_rows(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    lang: ddc_ui::Lang,
    on: bool,
    shown: Option<&str>,
) -> TokenOut {
    let mut o = TokenOut::default();
    let shown = shown.filter(|_| on);
    let secondary = th.c("text.secondary");
    form_row(ui, th, &crate::tr_in(lang, "mcp.set.token", &[]), |ui, _| {
        if !on {
            let off = crate::tr_in(lang, "mcp.set.token_off", &[]);
            ui.label(egui::RichText::new(off).color(secondary));
            return;
        }
        if let Some(t) = shown {
            let tip = crate::tr_in(lang, "mcp.set.copy", &[]);
            if ddc_ui::widgets::glyph_button(ui, "copy", true, &tip).clicked() {
                crate::ai::ui::keys::copy_secret(ui.ctx(), t.to_owned());
                o.copied = true;
            }
            let tip = crate::tr_in(lang, "mcp.set.hide", &[]);
            o.hide = ddc_ui::widgets::glyph_button(ui, "close", true, &tip).clicked();
        } else {
            ui.label(egui::RichText::new(mask_token()).monospace().color(secondary));
        }
        o.renew = Button::secondary(&crate::tr_in(lang, "mcp.set.new", &[]))
            .show(ui, th)
            .clicked();
    });
    if !on {
        return o;
    }
    if let Some(t) = shown {
        ui.horizontal(|ui| {
            ui.add_space(LABEL_W);
            ui.label(egui::RichText::new(t).monospace().color(th.c("text.primary")));
        });
        ui.horizontal(|ui| {
            ui.add_space(LABEL_W);
            crate::ui_kit::warn_note(ui, crate::tr_in(lang, "mcp.token.once", &[]));
        });
    }
    form_row(ui, th, &crate::tr_in(lang, "mcp.set.config", &[]), |ui, _| {
        o.copy_cfg = Button::secondary(&crate::tr_in(lang, "mcp.set.copy_config", &[]))
            .show(ui, th)
            .clicked();
    });
    o
}

/// Footer of the access window: one primary Close (click only, Esc closes too).
pub(crate) fn settings_footer(ui: &mut egui::Ui, lang: ddc_ui::Lang) -> bool {
    crate::ui_kit::dialog_footer(ui, |ui| {
        crate::ui_kit::primary_click(ui, crate::tr_in(lang, "dlg.close", &[]), None)
    })
}

/// The warning pill while agents apply in drafts without asking; true when its button was pressed.
fn auto_chip(ui: &mut egui::Ui, th: &ddc_ui::Theme, lang: ddc_ui::Lang) -> bool {
    ui.horizontal(|ui| {
        ddc_ui::widgets::badge(
            ui,
            th,
            &crate::tr_in(lang, "mcp.auto.chip", &[]),
            ddc_ui::widgets::Tone::Warning,
        )
        .on_hover_text(crate::tr_in(lang, "mcp.auto.chip_tip", &[]));
        Button::secondary(&crate::tr_in(lang, "mcp.auto.ask_again", &[]))
            .small()
            .show(ui, th)
            .clicked()
    })
    .inner
}

/// The running server with its queue. Holds nothing of the model.
pub struct McpBridge {
    handle: McpHandle,
    rx: mpsc::Receiver<McpCall>,
    store: Arc<dyn SecretStore>,
    pending: Vec<McpCall>,
    /// The token was issued by this start: the settings show it once.
    fresh: bool,
}

impl McpBridge {
    pub fn start(port: u16, store: Arc<dyn SecretStore>) -> Result<Self, String> {
        Self::start_with(port, store, McpLimits::default())
    }

    /// The token is issued here, on the person's enabling step, and only if there is none yet.
    pub fn start_with(
        port: u16,
        store: Arc<dyn SecretStore>,
        limits: McpLimits,
    ) -> Result<Self, String> {
        ddc_model::set_audit_scrubber(ddc_ai::secret::scrub_known);
        let (token, fresh) = match mcp::load_token(store.as_ref()).map_err(|e| e.to_string())? {
            Some(t) => (t, false),
            None => (
                mcp::issue_token(store.as_ref()).map_err(|e| e.to_string())?,
                true,
            ),
        };
        let (tx, rx) = mpsc::channel();
        let handle =
            mcp::serve_with(McpConfig { port, token }, limits, tx).map_err(|e| e.to_string())?;
        Ok(McpBridge {
            handle,
            rx,
            store,
            pending: Vec::new(),
            fresh,
        })
    }

    /// True once after a start that issued the token.
    pub fn take_fresh(&mut self) -> bool {
        std::mem::take(&mut self.fresh)
    }

    pub fn port(&self) -> u16 {
        self.handle.port()
    }

    pub fn clients(&self) -> Vec<ClientSeen> {
        self.handle.clients()
    }

    pub fn in_flight(&self) -> usize {
        self.handle.in_flight()
    }

    pub fn set_waker(&self, f: impl Fn() + Send + Sync + 'static) {
        self.handle.set_waker(f);
    }

    pub fn token(&self) -> Result<Option<Secret>, String> {
        mcp::load_token(self.store.as_ref()).map_err(|e| e.to_string())
    }

    pub fn has_calls(&mut self) -> bool {
        self.pending.extend(self.rx.try_iter());
        !self.pending.is_empty()
    }

    /// Dropped calls answer `busy` on the agent's side.
    pub fn drop_calls(&mut self) {
        self.pending.clear();
    }

    /// Once a frame on the UI thread: the policy is read per call from the live file.
    pub fn pump(&mut self, s: &mut UiSession, ed: &Editor, view: Option<ViewCtx>) {
        self.has_calls();
        for call in self.pending.drain(..) {
            s.mcp_call(ed, call, view.clone());
        }
        for name in self.handle.take_connects() {
            audit(
                ed,
                s.session_id(),
                &format!("{ACTOR_PREFIX}{}", fmt::text(&name)),
                "mcp_connect",
                None,
                "ok",
            );
        }
        for (name, uri) in self.handle.take_resource_reads() {
            let e = AuditEntry {
                session: s.session_id().to_string(),
                actor: format!("{ACTOR_PREFIX}{}", fmt::text(&name)),
                event: RESOURCE_READ.to_string(),
                endpoint: Some(uri),
                status: "ok".to_string(),
                ..AuditEntry::default()
            };
            crate::ai::session::audit_write(&ed.model, &e);
        }
    }

    /// A new token: the old one answers 401 from this moment, its queued calls do not run and an
    /// open changeset is rolled back.
    pub fn revoke(&self, s: Option<&UiSession>, ed: &Editor) -> Result<(), String> {
        let token = mcp::issue_token(self.store.as_ref()).map_err(|e| e.to_string())?;
        self.handle.set_token(token);
        if let Some(u) = s {
            u.mcp_cut();
        }
        audit(
            ed,
            s.map_or(NO_SESSION, |u| u.session_id()),
            "user",
            "mcp_policy",
            Some("revoke".to_string()),
            "ok",
        );
        Ok(())
    }
}

/// A session for an agent while the person has no chat open: it never calls a model.
struct NoModel;

impl Provider for NoModel {
    fn stream(
        &self,
        _req: &ChatRequest<'_>,
        _sink: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::Blocked(
            "no chat model is connected".to_string(),
        ))
    }

    fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        Ok(vec![])
    }
}

/// True while the current session is the bridge's own: the chat panel replaces it with a real one.
pub fn session_is_inert() -> bool {
    with_session(|s| {
        s.as_ref()
            .is_some_and(|u| u.session_id().starts_with(INERT_PREFIX))
    })
}

fn snapshots_allowed(ed: &Editor) -> bool {
    super::ui::connect::snapshots_on(&ed.model)
}

fn inert_cfg(allow_snapshots: Arc<AtomicBool>) -> SessionCfg {
    let mut profile = ddc_ai::fake::profile();
    profile.id = NO_SESSION.to_string();
    profile.name = NO_SESSION.to_string();
    profile.base_url = String::new();
    profile.model = String::new();
    profile.images = true;
    SessionCfg {
        provider: Arc::new(NoModel),
        profile,
        limits: Limits::default(),
        session: format!("{INERT_PREFIX}{}", model_epoch()),
        allow_snapshots,
        extra_generators: vec![],
        extra_rules: vec![],
        keep_text: false,
        confirm_ttl: super::session::CONFIRM_TTL,
        target: ddc_ai::catalog::Target::Main,
        auto_replan: false,
    }
}

fn view_of(app: &EditorApp) -> ViewCtx {
    ViewCtx {
        view: if app.view3d { "3d" } else { "plan" }.to_string(),
        level: app.ed.level,
        units: app.ed.tool.units,
        selection: app.ed.selection.clone(),
        bounds: None,
        ..ViewCtx::default()
    }
}

fn ensure_session(app: &EditorApp, allow: &Arc<AtomicBool>) -> Result<(), String> {
    let current = model_epoch();
    with_session(|slot| -> Result<(), String> {
        if slot.as_ref().is_some_and(|u| u.model_epoch() == current) {
            return Ok(());
        }
        *slot = None;
        let started = UiSession::start(&app.ed, inert_cfg(allow.clone()), view_of(app))
            .map_err(|e| e.to_string())?;
        *slot = Some(started);
        Ok(())
    })
}

/// A session for a window's own tool calls when no chat runs: no model, no snapshots, and its
/// writes wait in a draft for the person's Apply like any connection's.
pub(crate) fn ensure_tool_session(app: &EditorApp) -> Result<(), String> {
    if with_session(|s| s.is_some()) {
        return Ok(());
    }
    ensure_session(app, &Arc::new(AtomicBool::new(false)))
}

fn gesture_now(app: &EditorApp, ctx: &egui::Context) -> bool {
    ctx.input(|i| i.pointer.any_down())
        || ctx.dragged_id().is_some()
        || !app.ed.chain().is_empty()
        || app.ed.wall_draft().rect_start.is_some()
}

fn session_label() -> String {
    with_session(|s| {
        s.as_ref()
            .map_or_else(|| NO_SESSION.to_string(), |u| u.session_id().to_string())
    })
}

#[derive(Default)]
struct BridgeUi {
    loaded: bool,
    path: Option<PathBuf>,
    settings: McpSettings,
    bridge: Option<McpBridge>,
    error: Option<String>,
    settings_open: bool,
    feed_open: bool,
    token_shown: Option<String>,
    allow: Arc<AtomicBool>,
    feed: Vec<agent_view::FeedRow>,
    /// The agent the journal is narrowed to; `None` shows all.
    feed_agent: Option<String>,
    feed_at: Option<Instant>,
    /// The open question; the choice is carried out only on its yes.
    ask: Option<Ask>,
    ask_focus: bool,
    /// The model epoch whose stored policy was already reset to asking.
    guard_epoch: Option<u64>,
    toast: Option<&'static str>,
}

impl BridgeUi {
    fn load(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        self.path = crate::gfx::data_dir().map(|d| d.join(SETTINGS_FILE));
        if let Some(p) = &self.path {
            self.settings = McpSettings::load_from(p);
        }
    }

    fn save(&mut self) {
        if let Some(p) = &self.path {
            if let Err(e) = self.settings.save_to(p) {
                self.error = Some(e.to_string());
            }
        }
    }

    fn ensure_running(&mut self, ctx: &egui::Context) {
        if !self.settings.enabled || self.bridge.is_some() || self.error.is_some() {
            return;
        }
        match McpBridge::start(self.settings.port, ddc_ai::secret::default_store()) {
            Ok(mut b) => {
                if b.take_fresh() {
                    self.token_shown = b.token().ok().flatten().as_ref().map(mcp::reveal);
                }
                let c = ctx.clone();
                b.set_waker(move || c.request_repaint());
                self.bridge = Some(b);
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn stop(&mut self) {
        with_session(|s| {
            if let Some(u) = s.as_ref() {
                u.mcp_cut();
            }
        });
        self.bridge = None;
        self.token_shown = None;
    }

    /// Immediate: the old token stops working now and the new one is not shown.
    fn revoke(&mut self, ed: &Editor) -> Result<(), String> {
        self.token_shown = None;
        match &self.bridge {
            Some(b) => with_session(|s| b.revoke(s.as_ref(), ed)),
            None => mcp::issue_token(ddc_ai::secret::default_store().as_ref())
                .map(|_| ())
                .map_err(|e| e.to_string()),
        }
    }

    /// A new token after the person's yes: shown once, until hidden or the window closes.
    fn renew(&mut self, ed: &Editor) -> Result<(), String> {
        self.revoke(ed)?;
        let token = match &self.bridge {
            Some(b) => b.token()?,
            None => mcp::load_token(ddc_ai::secret::default_store().as_ref())
                .map_err(|e| e.to_string())?,
        };
        self.token_shown = token.as_ref().map(mcp::reveal);
        Ok(())
    }

    fn renew_with_toast(&mut self, app: &mut EditorApp) {
        match self.renew(&app.ed) {
            Ok(()) => self.toast = Some("mcp.renewed"),
            Err(e) => app.sh.toasts.push(
                Level::Error,
                crate::tr("mcp.revoke_failed", &[("error", e.as_str())]),
                None,
            ),
        }
    }

    fn open_ask(&mut self, a: Ask) {
        self.ask = Some(a);
        self.ask_focus = true;
    }

    /// Closes the question; only a yes hands it back to carry out.
    fn answer(&mut self, yes: bool) -> Option<Ask> {
        self.ask_focus = false;
        self.ask.take().filter(|_| yes)
    }

    /// What may be written now; applying without asking only opens the question.
    fn request_policy(&mut self, current: McpPolicy, want: McpPolicy) -> Option<McpPolicy> {
        match policy_step(current, want) {
            PolicyStep::Set(p) => Some(p),
            PolicyStep::Ask => {
                self.open_ask(Ask::AutoApply);
                None
            }
            PolicyStep::Keep => None,
        }
    }

    /// Once per model: a start or a newly opened model asks again.
    fn session_guard(&mut self, ed: &Editor) {
        let now = model_epoch();
        if self.guard_epoch == Some(now) {
            return;
        }
        self.guard_epoch = Some(now);
        if let Err(e) = session_reset(ed, &session_label()) {
            self.error = Some(e);
        }
    }

    fn revoke_with_toast(&mut self, app: &mut EditorApp) {
        match self.revoke(&app.ed) {
            Ok(()) => app
                .sh
                .toasts
                .push(Level::Success, crate::tr("mcp.revoked", &[]), None),
            Err(e) => app.sh.toasts.push(
                Level::Error,
                crate::tr("mcp.revoke_failed", &[("error", e.as_str())]),
                None,
            ),
        }
    }

    /// Every UI path goes through here, so none sets applying without asking in one click.
    fn switch_policy(&mut self, app: &mut EditorApp, policy: McpPolicy) {
        if let Some(p) = self.request_policy(live_policy(&app.ed), policy) {
            self.write_policy(app, p);
        }
    }

    fn write_policy(&mut self, app: &mut EditorApp, policy: McpPolicy) {
        if let Err(e) = set_policy(&app.ed, &session_label(), policy) {
            app.sh.toasts.push(
                Level::Error,
                crate::tr("mcp.policy_failed", &[("error", e.as_str())]),
                None,
            );
        }
    }

    fn pump(&mut self, app: &mut EditorApp, ctx: &egui::Context) {
        let Some(bridge) = self.bridge.as_mut() else {
            return;
        };
        if bridge.has_calls() {
            self.allow
                .store(snapshots_allowed(&app.ed), Ordering::SeqCst);
            if let Err(e) = ensure_session(app, &self.allow) {
                bridge.drop_calls();
                self.error = Some(e);
            }
        }
        let gesture = gesture_now(app, ctx);
        let view = view_of(app);
        with_session(|s| {
            if let Some(u) = s.as_mut() {
                u.set_gesture(gesture);
                bridge.pump(u, &app.ed, Some(view));
            }
        });
        session::pump(app, ctx);
        if bridge.in_flight() > 0 {
            ctx.request_repaint_after(WORK_FRAME);
        }
    }

    fn indicator(&mut self, app: &mut EditorApp, ctx: &egui::Context) {
        let Some(bridge) = &self.bridge else {
            return;
        };
        let clients = bridge.clients();
        let waiting = with_session(|s| s.as_ref().and_then(|u| u.agent_waiting()));
        let drafts = with_session(|s| s.as_ref().map_or(0, |u| u.agent_writes_waiting()));
        if !bar_visible(!clients.is_empty(), waiting.is_some(), drafts > 0) {
            return;
        }
        // the indicator goes off by itself when an agent stops calling, so frames must keep coming
        ctx.request_repaint_after(INDICATOR_FRAME);
        let label = with_session(|s| {
            let line = |c: &ClientSeen| {
                let n = s
                    .as_ref()
                    .map_or(0, |u| u.agent_batches(&fmt::text(&c.name)));
                let n = n.to_string();
                crate::tr(
                    "mcp.bar.agent",
                    &[("name", c.name.as_str()), ("n", n.as_str())],
                )
            };
            clients.iter().map(line).collect::<Vec<_>>().join(" · ")
        });
        let warning = app.sh.theme.c("warning");
        let (mut show, mut revoke, mut review) = (false, false, false);
        let (th, cat) = (&app.sh.theme, &app.sh.cat);
        let frame = egui::Frame::new()
            .fill(th.c("bg.panel"))
            .inner_margin(egui::Margin::symmetric(10, 3));
        egui::TopBottomPanel::bottom("ddc.mcp.bar")
            .frame(frame)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // a filled dot, not colour alone: the text beside it says the same
                    let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.0, warning);
                    if drafts > 0 {
                        let n = drafts.to_string();
                        let text = crate::tr("agent_draft.bar.drafts", &[("n", n.as_str())]);
                        ui.label(
                            egui::RichText::new(text)
                                .strong()
                                .color(th.c("text.primary")),
                        );
                        review |= Button::primary(&cat.t("agent_draft.bar.review"))
                            .small()
                            .show(ui, th)
                            .clicked();
                    }
                    if let Some(w) = &waiting {
                        let n = w.n.to_string();
                        ui.label(
                            egui::RichText::new(crate::tr("mcp.bar.waiting", &[("n", n.as_str())]))
                                .strong()
                                .color(th.c("text.primary")),
                        );
                        review |= Button::primary(&cat.t("mcp.bar.review")).small().show(ui, th).clicked();
                        if let Some(left) = w.left {
                            let t = agent_view::countdown(left);
                            ui.label(
                                egui::RichText::new(crate::tr("mcp.bar.expires", &[("time", t.as_str())]))
                                    .color(th.c("text.secondary")),
                            );
                        }
                    }
                    ui.label(egui::RichText::new(label).color(th.c("text.primary")));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        revoke = Button::danger(&cat.t("mcp.bar.revoke"))
                            .small()
                            .show(ui, th)
                            .clicked();
                        show = Button::quiet(&cat.t("mcp.bar.show")).small().show(ui, th).clicked();
                    });
                });
            });
        if review {
            super::ui::open_panel(app);
        }
        if show {
            self.feed_open = !self.feed_open;
        }
        if revoke {
            self.revoke_with_toast(app);
        }
    }

    fn refresh_feed(&mut self, ed: &Editor) {
        if self.feed_at.is_some_and(|t| t.elapsed() < FEED_REFRESH) {
            return;
        }
        self.feed_at = Some(Instant::now());
        let top: i64 = ed
            .model
            .sql()
            .query_row("SELECT coalesce(max(seq),0) FROM ai_audit", [], |r| {
                r.get(0)
            })
            .unwrap_or(0);
        let rows = ed
            .model
            .audit_list(top - FEED_ROWS, FEED_ROWS as usize)
            .unwrap_or_default();
        self.feed = agent_view::feed_rows(rows);
    }

    fn feed_window(&mut self, app: &EditorApp, ctx: &egui::Context) {
        if !self.feed_open {
            return;
        }
        self.refresh_feed(&app.ed);
        // live: the journal reads the file again every second while it is open
        ctx.request_repaint_after(FEED_REFRESH);
        let mut open = true;
        let (th, cat) = (&app.sh.theme, &app.sh.cat);
        let agents = agent_view::agents_in(&self.feed);
        if self.feed_agent.as_ref().is_some_and(|a| !agents.contains(a)) {
            self.feed_agent = None;
        }
        let mut pick = self
            .feed_agent
            .as_ref()
            .and_then(|a| agents.iter().position(|x| x == a))
            .map_or(0, |i| i + 1);
        let rows = agent_view::visible(&self.feed, self.feed_agent.as_deref());
        crate::ui_kit::Dialog::new("mcp.feed", cat.t("mcp.feed.title"), crate::ui_kit::DialogSize::Tool)
            .open(&mut open)
            .show(ctx, |ui| {
                if agents.len() > 1 {
                    let all = cat.t("mcp.feed.all_agents");
                    let mut names: Vec<&str> = vec![all.as_str()];
                    names.extend(agents.iter().map(String::as_str));
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(cat.t("mcp.feed.agent"))
                                .color(th.c("text.secondary")),
                        );
                        super::ui::kit::dropdown(ui, th, "ddc.mcp.feed_agent", &mut pick, &names, 180.0);
                    });
                }
                if rows.is_empty() {
                    ui.label(egui::RichText::new(cat.t("mcp.feed.empty")).color(th.c("text.secondary")));
                }
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for r in &rows {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(super::ui::logic::fmt_local(r.ts))
                                    .monospace()
                                    .color(th.c("text.secondary")),
                            );
                            let who = agent_view::agent_name(&r.actor);
                            let who = if who.is_empty() {
                                cat.t("ai.audit.src.person")
                            } else {
                                fmt::clip(&fmt::text(who), 40)
                            };
                            ui.label(egui::RichText::new(who).strong().color(th.c("text.primary")));
                            let mut line = cat.t(agent_view::event_key(r));
                            if let Some(k) = agent_view::status_key(&r.status) {
                                line = format!("{line} \u{b7} {}", cat.t(k));
                            }
                            ui.label(egui::RichText::new(line).color(th.c("text.primary")));
                        });
                    }
                });
            });
        self.feed_agent = pick.checked_sub(1).and_then(|i| agents.get(i)).cloned();
        self.feed_open = open;
    }

    fn draw_settings(&mut self, app: &mut EditorApp, ui: &mut egui::Ui) {
        let out = self.draw_access(ui, app.sh.theme.c("warning"), live_policy(&app.ed));
        if out.renew {
            self.revoke_with_toast(app);
        }
        if let Some(p) = out.policy {
            self.switch_policy(app, p);
        }
    }

    /// Switch, port, address, token and the confirmation choice. It needs no editor: what the
    /// person decided comes back for the caller to carry out.
    /// A new token and applying without asking open a question here; neither is done in one click.
    fn draw_access(&mut self, ui: &mut egui::Ui, _warn: egui::Color32, current: McpPolicy) -> AccessOut {
        let lang = if crate::ui_lang_now() {
            ddc_ui::Lang::Ru
        } else {
            ddc_ui::Lang::En
        };
        let live = self.bridge.as_ref().map(|b| Live {
            port: b.port(),
            clients: b.clients().len(),
        });
        self.draw_access_in(ui, current, lang, live)
    }

    /// The same in an explicit language; `live` is the running server, if any.
    fn draw_access_in(&mut self, ui: &mut egui::Ui, current: McpPolicy, lang: ddc_ui::Lang, live: Option<Live>) -> AccessOut {
        let mut out = AccessOut::default();
        self.load();
        let th = ddc_ui::theme::of(ui.ctx());
        let t = |k: &str| crate::tr_in(lang, k, &[]);
        ui.label(t("mcp.set.about"));
        ui.label(egui::RichText::new(t("mcp.set.safety")).color(th.c("text.secondary")));
        ui.add_space(ddc_ui::theme::space::SM);
        status_row(ui, &th, lang, self.settings.enabled, self.error.is_some(), live);
        let mut on = self.settings.enabled;
        let flipped = form_row(ui, &th, &t("mcp.set.access"), |ui, _| {
            ddc_ui::widgets::toggle(ui, &th, &mut on, &t("mcp.set.enable")).changed()
        });
        if flipped {
            self.settings.enabled = on;
            self.error = None;
            self.save();
            if !on {
                self.stop();
                if current == McpPolicy::AutoApply {
                    out.policy = Some(McpPolicy::Confirm);
                }
            }
        }
        if let Some(p) = port_row(ui, &th, lang, self.settings.port, self.settings.enabled).0 {
            self.settings.port = p;
            self.save();
        }
        if let Some(e) = &self.error {
            crate::ui_kit::error_note(ui, &crate::tr_in(lang, "mcp.set.error", &[("error", e.as_str())]));
        }
        if current == McpPolicy::AutoApply && auto_chip(ui, &th, lang) {
            out.policy = Some(McpPolicy::Confirm);
        }
        let tok = token_rows(ui, &th, lang, live.is_some(), self.token_shown.as_deref());
        let mut copied = tok.copied;
        if tok.copy_cfg {
            if let Some(bridge) = &self.bridge {
                let url = format!("http://127.0.0.1:{}/mcp", bridge.port());
                match bridge.token() {
                    Ok(Some(t)) => {
                        let cfg = client_config(&url, &mcp::reveal(&t));
                        crate::ai::ui::keys::copy_secret(ui.ctx(), cfg);
                        self.error = None;
                        copied = true;
                    }
                    Ok(None) => self.error = Some(crate::tr_in(lang, "mcp.set.no_token", &[])),
                    Err(e) => self.error = Some(e),
                }
            }
        }
        if tok.hide {
            self.token_shown = None;
        }
        if copied {
            self.toast = Some("ai_safety.copied_clears");
        }
        if tok.renew {
            self.open_ask(Ask::NewToken);
        }
        if live.is_none() {
            return out;
        }
        // no switch any more: an agent's writes always wait in its draft for the person's Apply
        form_row(ui, &th, &t("mcp.set.policy"), |ui, _| {
            ui.label(egui::RichText::new(t("agent_draft.set.drafts")).color(th.c("text.secondary")));
        });
        out
    }

    fn settings_window(&mut self, app: &mut EditorApp, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut close = false;
        let lang = if crate::ui_lang_now() {
            ddc_ui::Lang::Ru
        } else {
            ddc_ui::Lang::En
        };
        crate::ui_kit::Dialog::new("mcp.settings", crate::tr("mcp.set.title", &[]), crate::ui_kit::DialogSize::Form)
            .open(&mut open)
            .show(ctx, |ui| {
                self.draw_settings(app, ui);
                close = settings_footer(ui, lang);
            });
        self.set_settings_open(open && !close);
    }

    /// Closing the window forgets a revealed token, so the next open starts masked.
    fn set_settings_open(&mut self, open: bool) {
        self.settings_open = open;
        if !open {
            self.token_shown = None;
        }
    }

    /// Cancel is focused and takes Enter; Esc and the backdrop cancel too. The yes is danger styled.
    fn ask_window(&mut self, app: &mut EditorApp, ctx: &egui::Context) {
        let Some(ask) = self.ask else {
            return;
        };
        let (title, body, yes) = ask_keys(ask);
        let focus = std::mem::take(&mut self.ask_focus);
        let mut pick: Option<bool> = None;
        let (th, cat) = (&app.sh.theme, &app.sh.cat);
        let key = match ask {
            Ask::AutoApply => "mcp.ask.auto",
            Ask::NewToken => "mcp.ask.token",
        };
        let m = crate::ui_kit::modal_dialog(key, ctx, |ui| {
            ui.set_width(ddc_ui::theme::layout::DIALOG_CONFIRM);
            crate::ui_kit::modal_title(ui, cat.t(title));
            ui.label(egui::RichText::new(cat.t(body)).color(th.c("text.secondary")));
            crate::ui_kit::dialog_footer(ui, |ui| {
                if crate::ui_kit::confirm_danger_button(ui, cat.t(yes), true).clicked() {
                    pick = Some(true);
                }
                let cancel = crate::ui_kit::cancel_button(ui, cat.t("mcp.ask.cancel"));
                if focus {
                    cancel.request_focus();
                }
                if cancel.clicked() {
                    pick = Some(false);
                }
            });
        });
        if pick.is_none() && m.should_close() {
            pick = Some(false);
        }
        let Some(yes) = pick else {
            return;
        };
        match self.answer(yes) {
            Some(Ask::AutoApply) => self.write_policy(app, McpPolicy::AutoApply),
            Some(Ask::NewToken) => self.renew_with_toast(app),
            None => {}
        }
    }
}

/// Once a frame from the shell. Does nothing at all until the person has turned the server on.
pub fn frame(app: &mut EditorApp, ctx: &egui::Context) {
    UI.with(|st| {
        let mut st = st.borrow_mut();
        st.load();
        // before any call is pumped: a stored "apply without asking" never outlives its session
        st.session_guard(&app.ed);
        st.ensure_running(ctx);
        st.pump(app, ctx);
        st.indicator(app, ctx);
        st.feed_window(app, ctx);
        st.settings_window(app, ctx);
        st.ask_window(app, ctx);
        if let Some(k) = st.toast.take() {
            app.sh.toasts.push(Level::Success, crate::tr(k, &[]), None);
        }
    });
}

/// What the token field shows until a person presses Show: a fixed string, so it leaks neither
/// characters nor length.
pub fn mask_token() -> &'static str {
    "************"
}

/// Client config for the clipboard. It holds the real token, so it is only built on an explicit
/// click and is never displayed or logged.
pub fn client_config(url: &str, token: &str) -> String {
    serde_json::to_string_pretty(&json!({
        "mcpServers": {"ddc-editor": {
            "type": "http",
            "url": url,
            "headers": {"Authorization": format!("Bearer {token}")}
        }}
    }))
    .unwrap_or_default()
}

/// What the person did in the access controls.
#[derive(Default, Debug, PartialEq)]
pub struct AccessOut {
    /// A new token was asked for: the old one stops working.
    pub renew: bool,
    pub policy: Option<McpPolicy>,
}

/// The AI settings tab embeds these controls; the choices come back as `AccessOut`.
pub fn access_ui(ui: &mut egui::Ui, warn: egui::Color32, current: McpPolicy) -> AccessOut {
    UI.with(|st| st.borrow_mut().draw_access(ui, warn, current))
}

/// The settings page embeds this; the same controls open in their own window from the command.
pub fn settings_ui(app: &mut EditorApp, ui: &mut egui::Ui) {
    UI.with(|st| st.borrow_mut().draw_settings(app, ui));
}

/// For the panel's commands: removing the stored token alone would leave the server on the old one.
pub fn revoke_now(app: &mut EditorApp) {
    UI.with(|st| st.borrow_mut().revoke_with_toast(app));
}

pub fn set_policy_now(app: &mut EditorApp, policy: McpPolicy) {
    UI.with(|st| st.borrow_mut().switch_policy(app, policy));
}

pub fn register(app: &mut EditorApp) {
    app.register_command("ai.mcp_settings", |_| open_settings());
}

/// Opens the server settings window; the AI panel's settings tab calls it too.
pub fn open_settings() {
    UI.with(|st| st.borrow_mut().settings_open = true)
}

#[cfg(test)]
pub(crate) use tests::layout_probe;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_is_off_until_a_person_turns_it_on() {
        assert!(!McpSettings::default().enabled);
        let dir = std::env::temp_dir().join(format!("ddc-mcp-settings-{}", std::process::id()));
        let path = dir.join("mcp.json");
        assert!(!McpSettings::load_from(&path).enabled, "no file: off");
        let on = McpSettings {
            enabled: true,
            port: 50000,
        };
        on.save_to(&path).unwrap();
        assert_eq!(McpSettings::load_from(&path), on);
        std::fs::write(&path, "{broken").unwrap();
        assert!(!McpSettings::load_from(&path).enabled, "damaged file: off");
        std::fs::write(&path, "{\"enabled\":true,\"port\":1}").unwrap();
        assert_eq!(
            McpSettings::load_from(&path).port,
            DEFAULT_PORT,
            "no privileged ports"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_masked_token_leaks_nothing() {
        let token = "abcd1234efgh5678";
        let m = mask_token();
        assert!(!m.is_empty());
        assert_eq!(m.len(), mask_token().len());
        for i in 0..token.len() - 3 {
            assert!(!m.contains(&token[i..i + 4]));
        }
    }

    #[test]
    fn the_copied_config_points_at_the_server_with_a_bearer_header() {
        let c = client_config("http://127.0.0.1:47621/mcp", "tok123");
        let v: J = serde_json::from_str(&c).unwrap();
        let s = &v["mcpServers"]["ddc-editor"];
        assert_eq!(s["type"], "http");
        assert_eq!(s["url"], "http://127.0.0.1:47621/mcp");
        assert_eq!(s["headers"]["Authorization"], "Bearer tok123");
    }

    #[test]
    fn open_settings_opens_the_window() {
        std::thread::spawn(|| {
            assert!(!UI.with(|st| st.borrow().settings_open));
            open_settings();
            assert!(UI.with(|st| st.borrow().settings_open));
        })
        .join()
        .unwrap();
    }

    #[test]
    fn closing_settings_hides_token() {
        let mut ui = BridgeUi::default();
        ui.token_shown = Some("secret-token".into());
        ui.set_settings_open(true);
        assert!(ui.token_shown.is_some(), "open keeps the reveal");
        ui.set_settings_open(false);
        assert!(!ui.settings_open);
        assert!(ui.token_shown.is_none());
    }

    #[test]
    fn the_config_text_is_valid_json_and_the_mask_hides_the_token() {
        let c = client_config("http://127.0.0.1:1/mcp", "tok-xyz-987");
        let v: J = serde_json::from_str(&c).unwrap();
        assert_eq!(v["mcpServers"]["ddc-editor"]["type"], "http");
        assert!(!mask_token().contains("tok"));
    }

    #[test]
    fn the_settings_strings_exist_in_both_locales() {
        for lang in ["en", "ru"] {
            let p = format!(
                "{}/../ui/data/i18n/{lang}/mcp_tools.json",
                env!("CARGO_MANIFEST_DIR")
            );
            let v: J = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
            for k in ["mcp.set.hide", "mcp.set.copy_config", "mcp.set.no_token"] {
                assert!(v[k].as_str().is_some_and(|t| !t.is_empty()), "{lang} {k}");
            }
        }
    }

    #[test]
    fn every_agent_write_waits_in_a_draft_and_nothing_switches_that_off() {
        assert_eq!(McpPolicy::default(), McpPolicy::Confirm);
        assert!(BridgeUi::default().ask.is_none());
        use McpPolicy::{AutoApply, Confirm};
        assert_eq!(policy_step(Confirm, AutoApply), PolicyStep::Keep);
        assert_eq!(policy_step(AutoApply, Confirm), PolicyStep::Set(Confirm));
        assert_eq!(policy_step(Confirm, Confirm), PolicyStep::Keep);
        let mut ui = BridgeUi::default();
        assert_eq!(ui.request_policy(Confirm, AutoApply), None);
        assert!(ui.ask.is_none(), "not even a question");
    }

    #[test]
    fn an_old_auto_value_still_reads_as_a_draft() {
        let ed = Editor::new_in_memory().unwrap();
        assert_eq!(session_reset(&ed, "t"), Ok(false));
        set_policy(&ed, "t", McpPolicy::AutoApply).unwrap();
        let mut ui = BridgeUi::default();
        ui.session_guard(&ed);
        assert_eq!(live_policy(&ed), McpPolicy::Confirm);
        on_model_replaced();
        ui.session_guard(&ed);
        assert_eq!(live_policy(&ed), McpPolicy::Confirm);
        assert!(ui.error.is_none());
    }

    #[test]
    fn a_stored_auto_apply_from_an_earlier_session_asks_and_the_open_writes_nothing() {
        let ed = Editor::new_in_memory().unwrap();
        ed.model.set_ai_policy(POLICY_KEY, "auto").unwrap();
        assert_eq!(live_policy(&ed), McpPolicy::Confirm, "no yes in this session");
        let rows = ed.model.audit_list(0, 100).unwrap().len();
        let mut ui = BridgeUi::default();
        ui.session_guard(&ed);
        assert_eq!(ed.model.ai_policy(POLICY_KEY).unwrap().as_deref(), Some("auto"));
        assert_eq!(ed.model.audit_list(0, 100).unwrap().len(), rows, "no write on open");
        assert_eq!(live_policy(&ed), McpPolicy::Confirm);
        assert!(ui.error.is_none());
    }

    #[test]
    fn the_bar_stays_while_a_draft_waits_and_asking_back_on_asks_nothing() {
        assert!(bar_visible(false, false, true), "no agent connected, a draft still shows");
        assert!(!bar_visible(false, false, false));
        let mut ui = BridgeUi::default();
        assert_eq!(
            ui.request_policy(McpPolicy::AutoApply, McpPolicy::Confirm),
            Some(McpPolicy::Confirm)
        );
        assert!(ui.ask.is_none(), "turning asking back on asks nothing");
    }

    #[test]
    fn a_new_token_needs_a_yes_is_shown_once_and_revoke_is_immediate() {
        let ed = Editor::new_in_memory().unwrap();
        let store: Arc<dyn SecretStore> = Arc::new(ddc_ai::MemoryStore::new());
        let mut b = McpBridge::start(0, store).unwrap();
        assert!(b.take_fresh(), "the first start issued the token");
        assert!(!b.take_fresh());
        let first = b.token().unwrap().map(|t| mcp::reveal(&t));
        let mut ui = BridgeUi {
            bridge: Some(b),
            ..BridgeUi::default()
        };
        let now = |ui: &BridgeUi| ui.bridge.as_ref().unwrap().token().unwrap().map(|t| mcp::reveal(&t));

        ui.open_ask(Ask::NewToken);
        assert_eq!(ui.answer(false), None);
        assert_eq!(now(&ui), first, "cancel keeps the token");
        assert!(ui.token_shown.is_none());

        ui.open_ask(Ask::NewToken);
        assert_eq!(ui.answer(true), Some(Ask::NewToken));
        ui.renew(&ed).unwrap();
        let second = now(&ui);
        assert_ne!(second, first);
        assert_eq!(ui.token_shown, second, "shown right after issuing");
        ui.set_settings_open(false);
        assert!(ui.token_shown.is_none(), "and not again");

        ui.revoke(&ed).unwrap();
        assert_ne!(now(&ui), second, "the old token is replaced at once");
        assert!(ui.token_shown.is_none(), "a revoke shows nothing");
    }

    #[test]
    fn port_values_stay_in_the_user_range() {
        assert_eq!(port_of(47621.0), Some(47621));
        assert_eq!(port_of(80.0), None);
        assert_eq!(port_of(70000.0), None);
        assert_eq!(port_of(f64::NAN), None);
    }

    #[test]
    fn the_access_controls_use_kit_widgets_only() {
        let src = include_str!("mcp_bridge.rs");
        let start = src.find("fn draw_access_in(").unwrap();
        let end = start + src[start..].find("fn settings_window(").unwrap();
        let rows = src.find("pub(crate) fn status_text(").unwrap();
        let rows_end = rows + src[rows..].find("/// The running server with its queue").unwrap();
        for body in [&src[start..end], &src[rows..rows_end]] {
            for raw in ["ui.checkbox", "small_button", "colored_label", "egui::DragValue", "ui.monospace"] {
                assert!(!body.contains(raw), "{raw}");
            }
        }
    }

    const FAKE_TOKEN: &str = "FAKE-TOKEN-NOT-REAL-0123456789abcdefghijklmnop";

    #[derive(Default)]
    struct Shot {
        texts: Vec<String>,
        /// (label, disabled) of every interactive control.
        controls: Vec<(String, bool)>,
        /// Every painted text with its rect.
        placed: Vec<(String, egui::Rect)>,
    }

    impl Shot {
        fn rect(&self, s: &str) -> egui::Rect {
            self.placed
                .iter()
                .find(|p| p.0 == s)
                .unwrap_or_else(|| panic!("no text {s:?} in {:?}", self.texts))
                .1
        }
        fn has(&self, s: &str) -> bool {
            self.texts.iter().any(|t| t.contains(s))
        }
        fn control(&self, label: &str) -> Option<bool> {
            self.controls.iter().find(|c| c.0 == label).map(|c| c.1)
        }
    }

    fn gather(s: &egui::Shape, out: &mut Vec<String>) {
        match s {
            egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
            egui::Shape::Vec(v) => v.iter().for_each(|x| gather(x, out)),
            _ => {}
        }
    }

    fn gather_placed(s: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
        match s {
            egui::Shape::Text(t) => {
                let r = t.galley.rect.translate(t.pos.to_vec2());
                out.push((t.galley.text().to_string(), r));
            }
            egui::Shape::Vec(v) => v.iter().for_each(|x| gather_placed(x, out)),
            _ => {}
        }
    }

    /// Draws `add` headless at the width of the dialog with explicit fonts; no global language.
    fn shoot(mut add: impl FnMut(&mut egui::Ui, &ddc_ui::Theme)) -> Shot {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx.enable_accesskit();
        let mut th = ddc_ui::Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
        let mut shot = Shot::default();
        for _ in 0..3 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(460.0, 900.0))),
                ..egui::RawInput::default()
            };
            let out = ctx.run(raw, |ctx| {
                let _ = th.apply(ctx);
                egui::CentralPanel::default().show(ctx, |ui| {
                    let t = ddc_ui::theme::of(ui.ctx());
                    add(ui, &t);
                });
            });
            shot = Shot::default();
            for c in &out.shapes {
                gather(&c.shape, &mut shot.texts);
                gather_placed(&c.shape, &mut shot.placed);
            }
            if let Some(u) = out.platform_output.accesskit_update {
                for (_, n) in u.nodes {
                    if matches!(n.role(), egui::accesskit::Role::Button | egui::accesskit::Role::TextInput | egui::accesskit::Role::SpinButton) {
                        shot.controls.push((n.label().or_else(|| n.value()).unwrap_or("").to_string(), n.is_disabled()));
                    }
                }
            }
        }
        shot
    }

    fn access_shot(lang: ddc_ui::Lang, enabled: bool, error: Option<&str>, live: Option<Live>, shown: bool) -> Shot {
        let mut st = BridgeUi {
            loaded: true,
            settings: McpSettings { enabled, port: 47621 },
            error: error.map(str::to_owned),
            token_shown: shown.then(|| FAKE_TOKEN.to_string()),
            ..BridgeUi::default()
        };
        shoot(|ui, _| {
            let _ = st.draw_access_in(ui, McpPolicy::Confirm, lang, live);
        })
    }

    const LANGS: [ddc_ui::Lang; 2] = [ddc_ui::Lang::En, ddc_ui::Lang::Ru];
    const LIVE: Live = Live { port: 47621, clients: 2 };

    #[test]
    fn the_status_chip_names_each_state_in_both_languages() {
        use ddc_ui::widgets::Tone;
        for lang in LANGS {
            let off = status_text(lang, false, false, None);
            let starting = status_text(lang, true, false, None);
            let failed = status_text(lang, true, true, None);
            let on = status_text(lang, true, false, Some(LIVE));
            assert_eq!(off.1, Tone::Neutral);
            assert_eq!(starting.1, Tone::Warning);
            assert_eq!(failed.1, Tone::Danger);
            assert_eq!(on.1, Tone::Success);
            assert!(on.0.contains("127.0.0.1:47621"), "{lang:?} {}", on.0);
            let all = [&off.0, &starting.0, &failed.0, &on.0];
            for (i, a) in all.iter().enumerate() {
                assert!(!a.is_empty() && !a.contains('{'), "{lang:?} {a}");
                for b in &all[i + 1..] {
                    assert_ne!(a, b, "{lang:?}");
                }
            }
            assert_eq!(status_text(lang, false, true, None).1, Tone::Neutral, "off stays off");
        }
    }

    #[test]
    fn the_off_state_shows_a_status_and_no_token_en_ru() {
        for lang in LANGS {
            // a token left in memory must not be drawn while the server is off
            let s = access_shot(lang, false, None, None, true);
            let tr = |k: &str| crate::tr_in(lang, k, &[]);
            assert!(s.has(&tr("mcp.set.state_off")), "{lang:?} status");
            assert!(s.has(&tr("mcp.set.token_off")), "{lang:?} token reason");
            assert!(s.has(&tr("mcp.set.about")) && s.has(&tr("mcp.set.safety")), "{lang:?} about split");
            assert!(s.has(&tr("mcp.set.port_range")), "{lang:?} range hint");
            assert!(!s.has(FAKE_TOKEN) && !s.has("127.0.0.1"), "{lang:?} nothing of a server");
            assert_eq!(s.control(&tr("mcp.set.new")), None, "{lang:?} no new token while off");
            assert_eq!(s.control(&tr("mcp.set.copy_config")), None, "{lang:?} no config while off");
            assert!(!s.has(&tr("agent_draft.set.drafts")), "{lang:?} policy row only while on");
        }
    }

    #[test]
    fn the_on_state_shows_address_clients_and_enabled_rows_en_ru() {
        for lang in LANGS {
            let tr = |k: &str| crate::tr_in(lang, k, &[]);
            let s = access_shot(lang, true, None, Some(LIVE), false);
            assert!(s.has(&status_text(lang, true, false, Some(LIVE)).0), "{lang:?} status");
            assert!(s.has(&crate::tr_in(lang, "mcp.set.clients", &[("n", "2")])), "{lang:?} clients");
            assert!(s.has(mask_token()) && !s.has(&tr("mcp.set.token_off")), "{lang:?} masked token");
            assert_eq!(s.control(&tr("mcp.set.new")), Some(false), "{lang:?} new token enabled");
            assert_eq!(s.control(&tr("mcp.set.copy_config")), Some(false), "{lang:?} copy config enabled");
            assert!(s.has(&tr("agent_draft.set.drafts")), "{lang:?} drafts row, no switch");
            assert!(!s.has(&tr("mcp.set.confirm")), "{lang:?} the old switch is gone");
            let shown = access_shot(lang, true, None, Some(LIVE), true);
            assert!(shown.has(FAKE_TOKEN) && shown.has(&tr("mcp.token.once")), "{lang:?} fresh token");
        }
    }

    #[test]
    fn a_failed_start_reads_as_not_running_with_the_reason_en_ru() {
        for lang in LANGS {
            let s = access_shot(lang, true, Some("address in use"), None, false);
            assert!(s.has(&crate::tr_in(lang, "mcp.set.state_failed", &[])), "{lang:?}");
            assert!(s.has("address in use"), "{lang:?} reason");
            let starting = access_shot(lang, true, None, None, false);
            assert!(starting.has(&crate::tr_in(lang, "mcp.set.state_starting", &[])), "{lang:?}");
        }
    }

    #[test]
    fn the_port_field_is_compact_in_both_languages() {
        for lang in LANGS {
            let mut width = 0.0;
            let mut changed = Some(1);
            let _ = shoot(|ui, th| {
                let (c, r) = port_row(ui, th, lang, 47621, false);
                width = r.width();
                changed = c;
            });
            assert!(width > 0.0, "{lang:?} drawn");
            assert!(width <= 3.0 * ddc_ui::sem::control::HIT_MIN + 0.5, "{lang:?} port field {width}");
            assert_eq!(changed, None, "{lang:?} an idle frame changes no port");
        }
    }

    /// The whole window body: form and footer, as the dialog draws them.
    fn form_shot(lang: ddc_ui::Lang, on: bool, shown: bool) -> Shot {
        let mut st = BridgeUi {
            loaded: true,
            settings: McpSettings { enabled: on, port: 47621 },
            token_shown: shown.then(|| FAKE_TOKEN.to_string()),
            ..BridgeUi::default()
        };
        let live = on.then_some(LIVE);
        shoot(|ui, _| {
            let _ = st.draw_access_in(ui, McpPolicy::Confirm, lang, live);
            let _ = settings_footer(ui, lang);
        })
    }

    #[test]
    fn the_form_labels_share_one_left_edge_en_ru() {
        for lang in LANGS {
            for on in [false, true] {
                let s = form_shot(lang, on, false);
                let tr = |k: &str| crate::tr_in(lang, k, &[]);
                let mut keys =
                    vec!["mcp.set.status", "mcp.set.access", "mcp.set.port", "mcp.set.token"];
                if on {
                    keys.extend(["mcp.set.config", "mcp.set.policy"]);
                }
                let x0 = s.rect(&tr(keys[0])).min.x;
                for k in keys {
                    let r = s.rect(&tr(k));
                    assert!((r.min.x - x0).abs() < 0.5, "{lang:?} on={on} {k} at {}", r.min.x);
                    assert!(r.width() <= LABEL_W + 0.5, "{lang:?} on={on} {k} too wide");
                }
            }
        }
    }

    #[test]
    fn hints_are_their_own_lines_in_the_control_column_en_ru() {
        for lang in LANGS {
            for on in [false, true] {
                let s = form_shot(lang, on, false);
                let tr = |k: &str| crate::tr_in(lang, k, &[]);
                let port = s.rect(&tr("mcp.set.port"));
                let hint = s.rect(&tr("mcp.set.port_range"));
                assert!(hint.min.x >= port.min.x + LABEL_W - 0.5, "{lang:?} on={on} hint column");
                assert!(hint.min.y >= port.max.y - 0.5, "{lang:?} on={on} hint under its row");
                let token = s.rect(&tr("mcp.set.token"));
                if !on {
                    let why = s.rect(&tr("mcp.set.token_off"));
                    assert!(why.min.x >= token.min.x + LABEL_W - 0.5, "{lang:?} reason column");
                    assert!((why.center().y - token.center().y).abs() < 4.0, "{lang:?} one row");
                }
            }
        }
    }

    #[test]
    fn the_footer_has_close_and_the_token_stays_masked_en_ru() {
        for lang in LANGS {
            let close = crate::tr_in(lang, "dlg.close", &[]);
            for (on, shown) in [(false, false), (false, true), (true, false)] {
                let s = form_shot(lang, on, shown);
                assert_eq!(s.control(&close), Some(false), "{lang:?} on={on} close");
                let label = s.rect(&crate::tr_in(lang, "mcp.set.token", &[]));
                assert!(s.rect(&close).min.x > label.min.x + 100.0, "{lang:?} close at the right");
                assert!(!s.has(FAKE_TOKEN), "{lang:?} on={on} shown={shown} token leaked");
                assert_eq!(s.has(mask_token()), on, "{lang:?} on={on} mask");
            }
            // only a token made a moment ago is drawn, and the window says it is shown once
            let s = form_shot(lang, true, true);
            assert!(s.has(FAKE_TOKEN) && !s.has(mask_token()), "{lang:?} fresh token");
        }
    }

    #[test]
    fn the_strings_of_the_access_window_exist_in_both_locales() {
        for lang in LANGS {
            for k in [
                "mcp.set.status",
                "mcp.set.access",
                "mcp.set.config",
                "mcp.set.policy",
                "mcp.set.about",
                "mcp.set.safety",
                "mcp.set.state_off",
                "mcp.set.state_on",
                "mcp.set.state_starting",
                "mcp.set.state_failed",
                "mcp.set.clients",
                "mcp.set.port_range",
                "mcp.set.token_off",
            ] {
                let s = crate::tr_in(lang, k, &[]);
                assert!(!s.is_empty() && s != k, "{lang:?} {k}");
            }
        }
        assert_ne!(
            crate::tr_in(ddc_ui::Lang::En, "mcp.set.state_off", &[]),
            crate::tr_in(ddc_ui::Lang::Ru, "mcp.set.state_off", &[])
        );
    }

    /// Fresh settings window for the layout contract (see `layout_contract`).
    pub(crate) fn layout_probe(lang: ddc_ui::Lang, on: bool) -> crate::layout_contract::Probe {
        let mut st = BridgeUi {
            loaded: true,
            settings: McpSettings { enabled: on, port: 47621 },
            token_shown: on.then(|| FAKE_TOKEN.to_string()),
            ..BridgeUi::default()
        };
        let live = on.then_some(LIVE);
        crate::layout_contract::probe("mcp.settings", |ctx| {
            crate::ui_kit::Dialog::new("mcp.settings", crate::tr_in(lang, "mcp.set.title", &[]), crate::ui_kit::DialogSize::Form)
                .show(ctx, |ui| {
                    let _ = st.draw_access_in(ui, McpPolicy::Confirm, lang, live);
                    let _ = settings_footer(ui, lang);
                });
        })
    }

    #[test]
    fn the_safety_strings_exist_in_both_locales() {
        for lang in ["en", "ru"] {
            let p = format!(
                "{}/../ui/data/i18n/{lang}/ai-mcp.json",
                env!("CARGO_MANIFEST_DIR")
            );
            let v: J = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
            let mut keys = vec![
                "mcp.ask.cancel",
                "mcp.auto.chip",
                "mcp.auto.chip_tip",
                "mcp.auto.ask_again",
                "mcp.token.once",
                "mcp.renewed",
                "mcp.copied",
            ];
            for a in [Ask::AutoApply, Ask::NewToken] {
                let (t, b, y) = ask_keys(a);
                keys.extend([t, b, y]);
            }
            for k in keys {
                assert!(v[k].as_str().is_some_and(|t| !t.is_empty()), "{lang} {k}");
            }
        }
    }

    #[test]
    fn the_epoch_of_another_thread_is_not_ours() {
        let before = model_epoch();
        std::thread::spawn(on_model_replaced).join().unwrap();
        assert_eq!(model_epoch(), before);
        on_model_replaced();
        assert_eq!(model_epoch(), before + 1);
    }
}
