//! Connections, project policy and the MCP policy. The key goes from a password field straight to
//! the OS store; afterwards the form only knows whether one is stored. No `Debug` on this state.

use super::kit::{self, space, Button, Dialog, DialogSize};
use super::{connect, keys, logic, Action, Cx, PanelUi};
use crate::ai::AiError;
use ddc_ai::{
    build_provider, consent_key, AuthKind, ChatRequest, EgressGuard, Executor, Flow, Message, Profile,
    Protocol, Role, Secret, SecretStore, StreamEvent, ToolCalling, ToolSchema,
};
use crate::units::{self, DecimalMark, NumError};
use ddc_model::Model;
use ddc_ui::theme::Role as FontRole;
use egui::{self, TextEdit, Ui};
use std::sync::mpsc::{self, Receiver};

const TAB_CONNECTIONS: usize = 0;
const TAB_PROJECT: usize = 1;
const PING_OUTPUT_TOKENS: u32 = 64;

enum JobMsg {
    Tested { tool: bool },
    Models(Vec<String>),
    Failed {
        key: &'static str,
        detail: Option<String>,
    },
}

/// What the result line under the form shows; every end of a job maps to one.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    Ok(&'static str),
    /// `detail` is the masked provider text: HTTP status, message, retry-after.
    Failed {
        key: &'static str,
        detail: Option<String>,
    },
}

/// The end of a job as the form sees it.
#[derive(Debug, PartialEq, Eq)]
enum Settled {
    Models(Vec<String>, Outcome),
    Done(Outcome),
}

/// Pure map from the channel's answer to the UI state; `None` while the job still runs.
fn settle(r: Result<JobMsg, mpsc::TryRecvError>) -> Option<Settled> {
    match r {
        Ok(JobMsg::Tested { tool }) => Some(Settled::Done(Outcome::Ok(if tool {
            "ai.settings.test_ok"
        } else {
            "ai.settings.test_no_tool"
        }))),
        Ok(JobMsg::Models(list)) => Some(Settled::Models(list, Outcome::Ok("ai.settings.models_ok"))),
        Ok(JobMsg::Failed { key, detail }) => Some(Settled::Done(Outcome::Failed { key, detail })),
        Err(mpsc::TryRecvError::Empty) => None,
        // The thread died without sending: it panicked.
        Err(mpsc::TryRecvError::Disconnected) => Some(Settled::Done(Outcome::Failed {
            key: "ai.settings.job_crashed",
            detail: None,
        })),
    }
}

fn spawn_failed() -> Outcome {
    Outcome::Failed {
        key: "ai.settings.job_spawn_failed",
        detail: None,
    }
}

#[derive(Default)]
pub struct Settings {
    loaded: bool,
    tab: usize,
    draft: Vec<Profile>,
    sel: usize,
    headers: String,
    /// What is being typed; emptied on save and on close.
    key: String,
    key_stored: bool,
    price_in: String,
    price_out: String,
    /// The executor role: model (empty: the main model runs every step) and its prices.
    exec_model: String,
    exec_price_in: String,
    exec_price_out: String,
    models: Vec<String>,
    job: Option<Receiver<JobMsg>>,
    outcome: Option<Outcome>,
    save_error: Option<String>,
    local_only: bool,
    snapshots: bool,
    auto_snap: bool,
    critic: bool,
    refine: bool,
    mep_auto: bool,
    keep_text: bool,
    max_cost: String,
    max_tokens: String,
    /// Refusal of the last typed cost / token limit (catalog key); the typed text stays.
    limit_err: [Option<&'static str>; 2],
    consents: Vec<String>,
    /// Set by the hosted preset: the address field takes focus on the next frame.
    focus_url: bool,
    /// Profile ids whose advanced section opens by itself (the custom preset).
    advanced_open: Vec<String>,
    /// Keys found in an address that the store refused on load, by profile id. Memory only:
    /// Save stores them before any list is written, so a connection never loses its key.
    held: Vec<(String, String)>,
}

fn key_field_id() -> egui::Id {
    egui::Id::new("ddc.ai.key_field")
}

fn headers_field_id() -> egui::Id {
    egui::Id::new("ddc.ai.headers_field")
}

fn url_field_id() -> egui::Id {
    egui::Id::new("ddc.ai.url_field")
}

/// A number in the UI's format: the point of the stored form becomes the UI's decimal mark.
fn amount_text(v: f64, mark: DecimalMark) -> String {
    let s = v.to_string();
    match mark {
        DecimalMark::Comma => s.replace('.', ","),
        DecimalMark::Point => s,
    }
}

fn price_text(v: Option<f64>, mark: DecimalMark) -> String {
    v.map(|x| amount_text(x, mark)).unwrap_or_default()
}

/// A stored limit shown the way the person types it.
fn stored_text(raw: Option<String>, mark: DecimalMark) -> String {
    let raw = raw.unwrap_or_default();
    match units::parse_locale_number(&raw, DecimalMark::Point) {
        Ok(v) => amount_text(v, mark),
        Err(_) => raw,
    }
}

/// A limit or a price typed in the UI's number format. `Ok(None)` for an empty field; the error
/// is the catalog key of the refusal. Zero is allowed (a limit of 0 means no limit).
fn parse_amount(text: &str, mark: DecimalMark) -> Result<Option<f64>, &'static str> {
    match units::parse_locale_number(text.trim(), mark) {
        Ok(v) if v < 0.0 => Err("ai.settings.num_negative"),
        Ok(v) => Ok(Some(v)),
        Err(NumError::Empty) => Ok(None),
        Err(NumError::CommaDecimal) => Err("ai.settings.num_comma"),
        Err(NumError::NotNumber) => Err("ai.settings.num_bad"),
    }
}

/// Writes a typed limit to the project policy in the stored form (point). `Ok(true)` when a
/// value was saved, `Ok(false)` for an empty field; on a refusal the old value stays.
fn commit_limit(m: &Model, key: &str, text: &str, mark: DecimalMark) -> Result<bool, &'static str> {
    match parse_amount(text, mark)? {
        Some(v) => Ok(m.set_ai_policy(key, &v.to_string()).is_ok()),
        None => Ok(false),
    }
}

/// The executor of a profile from the form: an empty model field means none (every step on the
/// main model). A refused price keeps the old one, as the main prices do.
fn executor_of(
    model: &str,
    price_in: &str,
    price_out: &str,
    old: Option<Executor>,
    mark: DecimalMark,
) -> Option<Executor> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    let old = old.unwrap_or_default();
    Some(Executor {
        model: model.to_string(),
        price_in_per_mtok: parse_amount(price_in, mark).unwrap_or(old.price_in_per_mtok),
        price_out_per_mtok: parse_amount(price_out, mark).unwrap_or(old.price_out_per_mtok),
    })
}

/// The grant list of this project, read when the window opens and after a change.
fn consents_of(m: &Model) -> Vec<String> {
    const PREFIX: &str = "egress.consent.";
    let read = || -> rusqlite::Result<Vec<String>> {
        let mut st = m.sql().prepare("SELECT key FROM ai_policy WHERE key LIKE 'egress.consent.%' AND value='1' ORDER BY key")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect()
    };
    read()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|k| k.strip_prefix(PREFIX).map(str::to_string))
        .collect()
}

impl Settings {
    fn load(&mut self, st_profiles: &[Profile], sel: usize, m: &Model) {
        self.loaded = true;
        self.draft = st_profiles.to_vec();
        self.held = take_held(&mut self.draft);
        self.sel = sel.min(self.draft.len().saturating_sub(1));
        self.save_error = None;
        self.local_only = connect::policy_flag(m, "egress.local_only", false);
        self.snapshots = connect::snapshots_on(m);
        self.auto_snap = connect::policy_flag(m, "ai.auto_snapshots", true);
        self.critic = connect::policy_flag(m, "ai.critic", true);
        self.refine = connect::policy_flag(m, crate::ai::project_tools::POLICY_REFINE, true);
        self.mep_auto = crate::ai::mep_gate::is_on_now(m);
        self.keep_text = connect::policy_flag(m, "audit.keep_text", false);
        let mark = units::ui_decimal_mark();
        self.max_cost = stored_text(m.ai_policy("limits.max_cost").ok().flatten(), mark);
        self.max_tokens = stored_text(m.ai_policy("limits.max_turn_tokens").ok().flatten(), mark);
        self.limit_err = [None; 2];
        self.consents = consents_of(m);
        self.load_form();
    }

    /// The form of the selected profile; the store is asked here, not per frame.
    fn load_form(&mut self) {
        self.key.clear();
        self.models.clear();
        self.outcome = None;
        match self.draft.get(self.sel) {
            Some(p) => {
                self.headers = logic::headers_text(&p.extra_headers);
                let mark = units::ui_decimal_mark();
                self.price_in = price_text(p.price_in_per_mtok, mark);
                self.price_out = price_text(p.price_out_per_mtok, mark);
                let e = p.executor.clone().unwrap_or_default();
                self.exec_model = e.model;
                self.exec_price_in = price_text(e.price_in_per_mtok, mark);
                self.exec_price_out = price_text(e.price_out_per_mtok, mark);
                self.key_stored = connect::store().get(&p.id).ok().flatten().is_some();
                if let Some((_, k)) = self.held.iter().find(|(id, _)| *id == p.id) {
                    self.key = k.clone();
                }
            }
            None => {
                self.headers.clear();
                self.price_in.clear();
                self.price_out.clear();
                self.exec_model.clear();
                self.exec_price_in.clear();
                self.exec_price_out.clear();
                self.key_stored = false;
            }
        }
    }

    /// A new connection from a preset becomes the selected one.
    fn add_preset(&mut self, preset: connect::Preset, cat: &ddc_ui::Catalog) {
        self.commit_form();
        let ids: Vec<String> = self.draft.iter().map(|p| p.id.clone()).collect();
        let number = self.draft.len() + 1;
        let name = cat.tf("ai.settings.new_name", &[("n", &number.to_string())]);
        let p = connect::preset_profile(preset, logic::profile_id(&ids), name);
        if preset == connect::Preset::Custom {
            self.advanced_open.push(p.id.clone());
        }
        self.focus_url = preset != connect::Preset::Local;
        self.draft.push(p);
        self.sel = self.draft.len() - 1;
        self.load_form();
    }

    /// What blocks a test of the selected connection, as catalog keys.
    fn missing(&self) -> Vec<&'static str> {
        self.draft.get(self.sel).map_or_else(Vec::new, |p| {
            connect::missing(p, self.key_stored || !self.key.is_empty())
        })
    }

    /// Why Save is off: a key in the headers box, or one left in another connection's headers.
    fn save_blocked(&self) -> Option<&'static str> {
        let others: Vec<Profile> = self
            .draft
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != self.sel)
            .map(|(_, p)| p.clone())
            .collect();
        let mark = units::ui_decimal_mark();
        keys::save_block(&self.headers)
            .or_else(|| keys::profiles_block(&others))
            .or_else(|| parse_amount(&self.price_in, mark).err())
            .or_else(|| parse_amount(&self.price_out, mark).err())
            .or_else(|| parse_amount(&self.exec_price_in, mark).err())
            .or_else(|| parse_amount(&self.exec_price_out, mark).err())
    }

    fn close(&mut self, ctx: &egui::Context) {
        self.key.clear();
        self.held.clear();
        self.loaded = false;
        self.job = None;
        forget_key_field(ctx);
    }

    /// Puts the form's text fields into the profile they belong to.
    fn commit_form(&mut self) {
        let mark = units::ui_decimal_mark();
        let (headers, price_in, price_out) = (
            logic::parse_headers(&self.headers),
            parse_amount(&self.price_in, mark),
            parse_amount(&self.price_out, mark),
        );
        if let Some(p) = self.draft.get_mut(self.sel) {
            lift_url_key(p, &mut self.key);
            p.extra_headers = headers;
            // a refused price keeps the old one; the form shows why
            if let Ok(v) = price_in {
                p.price_in_per_mtok = v;
            }
            if let Ok(v) = price_out {
                p.price_out_per_mtok = v;
            }
            p.executor = executor_of(
                &self.exec_model,
                &self.exec_price_in,
                &self.exec_price_out,
                p.executor.take(),
                mark,
            );
            // a held key follows the field: an emptied field lets it go
            let id = p.id.clone();
            if let Some(h) = self.held.iter_mut().find(|(i, _)| *i == id) {
                h.1.clone_from(&self.key);
            }
            self.held.retain(|(_, k)| !k.is_empty());
        }
    }

    fn save(&mut self, ctx: &egui::Context, cat: &ddc_ui::Catalog, acts: &mut Vec<Action>) {
        // the button is off then; this guards a keyboard save
        if self.save_blocked().is_some() {
            return;
        }
        self.commit_form();
        if keys::profiles_block(&self.draft).is_some() {
            return;
        }
        self.save_error = None;
        match self.save_with(&**connect::store(), connect::save_profiles) {
            Ok(()) => {}
            Err(SaveErr::Key) => {
                self.save_error = Some(cat.t("ai.settings.key_store_failed"));
                return;
            }
            Err(SaveErr::Profiles(e)) => {
                let why = keys::safe_error(&e);
                self.save_error = Some(cat.tf("ai.settings.save_failed", &[("why", &why)]));
                return;
            }
        }
        forget_key_field(ctx);
        acts.push(Action::ProfilesSaved(self.draft.clone(), self.sel));
    }

    /// The keys first, then the list: a key the store refuses stays in the field (or held) and
    /// no connection is written without it. The field empties only after both succeed.
    fn save_with(
        &mut self,
        store: &dyn SecretStore,
        write: impl FnOnce(&[Profile]) -> Result<(), String>,
    ) -> Result<(), SaveErr> {
        let sel_id = self
            .draft
            .get(self.sel)
            .map(|p| p.id.clone())
            .unwrap_or_default();
        if !self.key.is_empty() {
            if store.set(&sel_id, Secret::new(self.key.clone())).is_err() {
                return Err(SaveErr::Key);
            }
            self.key_stored = true;
        }
        let draft = &self.draft;
        let others = self
            .held
            .iter()
            .filter(|(id, _)| *id != sel_id && draft.iter().any(|p| p.id == *id));
        for (id, k) in others {
            if store.set(id, Secret::new(k.clone())).is_err() {
                return Err(SaveErr::Key);
            }
        }
        write(&self.draft).map_err(SaveErr::Profiles)?;
        self.key.clear();
        self.held.clear();
        Ok(())
    }

    fn poll(&mut self) {
        let Some(rx) = &self.job else {
            return;
        };
        match settle(rx.try_recv()) {
            None => {}
            Some(Settled::Models(list, o)) => {
                self.models = list;
                self.outcome = Some(o);
                self.job = None;
            }
            Some(Settled::Done(o)) => {
                self.outcome = Some(o);
                self.job = None;
            }
        }
    }

    /// Off the UI thread: a slow address must not freeze the editor. A test sends only its own
    /// ping, nothing of the project, so the card's consent is not needed for it.
    fn spawn(&mut self, ctx: &egui::Context, list_models: bool) {
        self.commit_form();
        let Some(p) = self.draft.get(self.sel).cloned() else {
            return;
        };
        let typed = (!self.key.is_empty()).then(|| Secret::new(self.key.clone()));
        let local_only = self.local_only;
        let (tx, rx) = mpsc::channel();
        self.job = Some(rx);
        self.outcome = None;
        let ctx = ctx.clone();
        // network probe only; a crash drops the sender and settle() reports job_crashed
        let wake = ctx.clone();
        let started = crate::jobs::spawn_guarded(
            "ai-settings",
            move || {
                let msg = if list_models {
                    fetch_models(&p, typed, local_only)
                } else {
                    ping(&p, typed, local_only)
                };
                let _ = tx.send(msg);
                ctx.request_repaint();
            },
            move |_| wake.request_repaint(),
        );
        if started.is_err() {
            self.job = None;
            self.outcome = Some(spawn_failed());
        }
    }
}

/// The edit state of the password field may keep a copy for its undo; the headers box too, once
/// a key was pasted there.
fn forget_key_field(ctx: &egui::Context) {
    keys::forget(ctx, key_field_id());
    keys::forget(ctx, headers_field_id());
    keys::forget(ctx, url_field_id());
}

/// The connections tab with a key sent in the address, for the layout contract.
#[cfg(test)]
pub(crate) fn layout_probe_query(lang: ddc_ui::Lang) -> crate::layout_contract::Probe {
    let ed = crate::editor::Editor::new_in_memory().unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let th = ddc_ui::Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    let mut st = PanelUi::default();
    let mut p = connect::blank_profile("probe-query".into(), "Probe".into());
    p.base_url = "https://h.test/v1".into();
    p.model = "m".into();
    p.auth = AuthKind::Query { name: "key".into() };
    st.settings.advanced_open.push(p.id.clone());
    st.profiles = vec![p];
    st.settings_open = true;
    let cx = Cx {
        th: &th,
        cat: &cat,
        ed: &ed,
        view3d: false,
        palette_open: false,
    };
    crate::layout_contract::probe("ai.settings", |ctx| {
        window(ctx, &mut st, &cx, &mut Vec::new());
    })
}

/// A connection loaded with a key still in its address (the store refused it on load).
#[cfg(test)]
pub(crate) fn layout_probe_held(lang: ddc_ui::Lang, key: &str) -> crate::layout_contract::Probe {
    let ed = crate::editor::Editor::new_in_memory().unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let th = ddc_ui::Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    let mut st = PanelUi::default();
    let mut p = connect::blank_profile("probe-held".into(), "Probe".into());
    p.base_url = format!("https://h.test/v1?key={key}");
    p.model = "m".into();
    st.profiles = vec![p];
    st.settings_open = true;
    let cx = Cx {
        th: &th,
        cat: &cat,
        ed: &ed,
        view3d: false,
        palette_open: false,
    };
    crate::layout_contract::probe("ai.settings", |ctx| {
        window(ctx, &mut st, &cx, &mut Vec::new());
    })
}

/// The note under the key field where the store keeps keys only in memory.
fn session_only_note(session_only: bool) -> Option<&'static str> {
    session_only.then_some("ai_safety.key_session_only")
}

enum SaveErr {
    Key,
    Profiles(String),
}

/// Keys still in an address after load (the store refused them): out of the address, into
/// memory by profile id, so the form can offer them again and Save can store them.
fn take_held(draft: &mut [Profile]) -> Vec<(String, String)> {
    let mut held = Vec::new();
    for p in draft {
        let mut k = String::new();
        lift_url_key(p, &mut k);
        if !k.is_empty() {
            held.push((p.id.clone(), k));
        }
    }
    held
}

/// The note under the key field of a connection whose key the store has not taken yet.
fn held_note(held: bool) -> Option<&'static str> {
    held.then_some("ai.settings.key_held")
}

/// A key typed into the address moves to the key field (a typed key wins). A query key that
/// is taken goes out as the same parameter, never as a bearer or header.
fn lift_url_key(p: &mut Profile, key: &mut String) {
    let Some(s) = ddc_ai::profile::split_url_secret(&p.base_url) else {
        return;
    };
    p.base_url = s.clean;
    let used = key.is_empty() && !s.value.is_empty();
    ddc_ai::profile::adopt_url_param(p, s.param.as_deref(), used);
    if used {
        *key = s.value;
    }
}

fn failed(e: &ddc_ai::ProviderError) -> JobMsg {
    // Known keys go first, so a short typed key is hidden too; the pattern mask comes after.
    let err = AiError::Provider(ddc_ai::secret::scrub_known(&e.to_string()));
    JobMsg::Failed {
        key: logic::err_key(logic::classify(&err)),
        detail: logic::err_detail(&err),
    }
}

fn provider_for(
    p: &Profile,
    typed: Option<Secret>,
    local_only: bool,
) -> Result<std::sync::Arc<dyn ddc_ai::Provider>, ddc_ai::ProviderError> {
    let key = if p.auth == AuthKind::None {
        None
    } else {
        typed.or_else(|| connect::store().get(&p.id).ok().flatten())
    };
    let host = consent_key(&p.base_url)?;
    build_provider(
        p,
        key,
        EgressGuard {
            local_only,
            consented_host: Some(host),
        },
    )
}

fn fetch_models(p: &Profile, typed: Option<Secret>, local_only: bool) -> JobMsg {
    match provider_for(p, typed, local_only).and_then(|x| x.list_models()) {
        Ok(list) => JobMsg::Models(list),
        Err(e) => failed(&e),
    }
}

/// One line of text and one tool on offer: tells apart "answers" from "can call tools".
fn ping(p: &Profile, typed: Option<Secret>, local_only: bool) -> JobMsg {
    let provider = match provider_for(p, typed, local_only) {
        Ok(x) => x,
        Err(e) => return failed(&e),
    };
    let messages = [Message::text(
        Role::User,
        "Connection test. Call the ping tool once.",
    )];
    let tools = [ToolSchema {
        name: "ping".to_string(),
        description: "Connection test".to_string(),
        input_schema: serde_json::json!({ "type": "object", "properties": {} }),
    }];
    let req = ChatRequest {
        model: &p.model,
        messages: &messages,
        tools: &tools,
        max_output_tokens: PING_OUTPUT_TOKENS,
        temperature: None,
    };
    let mut tool = false;
    let r = provider.stream(&req, &mut |e| {
        tool |= matches!(e, StreamEvent::ToolStart { .. });
        Flow::Continue
    });
    match r {
        Ok(()) => JobMsg::Tested { tool },
        Err(e) => failed(&e),
    }
}

fn row(ui: &mut Ui, cx: &Cx<'_>, label: &str, add: impl FnOnce(&mut Ui)) {
    row_tip(ui, cx, label, None, add);
}

/// A row whose label explains, on hover, when to change the field.
fn row_tip(ui: &mut Ui, cx: &Cx<'_>, label: &str, tip: Option<&str>, add: impl FnOnce(&mut Ui)) {
    let r = ui.label(
        egui::RichText::new(label)
            .font(cx.th.font(FontRole::Label))
            .color(cx.th.c("text.secondary")),
    );
    if let Some(t) = tip {
        r.on_hover_text(t);
    }
    add(ui);
    ui.end_row();
}

/// A combo taking this share of what is left of the row.
fn pick(ui: &mut Ui, cx: &Cx<'_>, salt: &str, i: &mut usize, items: &[&str], share: f32) -> bool {
    let w = ui.available_width() * share;
    kit::dropdown(ui, cx.th, salt, i, items, w)
}

/// A number box: right-aligned, a muted unit after it, the refusal (if any) under it.
fn number_field(
    ui: &mut Ui,
    cx: &Cx<'_>,
    value: &mut String,
    unit_key: &str,
    err: Option<&str>,
) -> egui::Response {
    ui.vertical(|ui| {
        let r = ui
            .horizontal(|ui| {
                let r = ui.add(
                    TextEdit::singleline(value)
                        .horizontal_align(egui::Align::Max)
                        .desired_width(space::XXXL * 2.0),
                );
                kit::caption(ui, cx.th, &cx.cat.t(unit_key));
                r
            })
            .inner;
        if let Some(k) = err {
            kit::error_note(ui, cx.th, &cx.cat.t(k));
        }
        r
    })
    .inner
}

fn text_field(ui: &mut Ui, value: &mut String) {
    ui.add(TextEdit::singleline(value).desired_width(f32::INFINITY));
}

fn form(ui: &mut Ui, s: &mut Settings, cx: &Cx<'_>, ctx: &egui::Context) {
    let (th, cat) = (cx.th, cx.cat);
    let mark = units::ui_decimal_mark();
    let (sel, stored) = (s.sel, s.key_stored);
    let focus_url = std::mem::take(&mut s.focus_url);
    let Some(p) = s.draft.get_mut(sel) else {
        return;
    };
    let open_advanced = s.advanced_open.contains(&p.id);
    let held = s.held.iter().any(|(id, _)| *id == p.id);
    let mut forget_key = false;
    let mut move_key = false;
    // The three things a first connection needs; the rest waits under "Advanced".
    egui::Grid::new("ddc.ai.settings_form")
        .num_columns(2)
        .spacing([space::SM, space::XS])
        .striped(false)
        .show(ui, |ui| {
            row(ui, cx, &cat.t("ai.settings.name"), |ui| {
                text_field(ui, &mut p.name)
            });
            row(ui, cx, &cat.t("ai.settings.base_url"), |ui| {
                let r = ui.add(
                    TextEdit::singleline(&mut p.base_url)
                        .id(url_field_id())
                        .desired_width(f32::INFINITY)
                        .hint_text(cat.t("ai.settings.base_url_hint")),
                );
                if focus_url {
                    r.request_focus();
                }
                // a pasted address with its key: the key leaves the visible field at once
                if r.lost_focus() || (r.changed() && !r.has_focus()) {
                    lift_url_key(p, &mut s.key);
                }
            });
            row(ui, cx, &cat.t("ai.settings.model"), |ui| {
                if s.models.is_empty() {
                    ui.add(
                        TextEdit::singleline(&mut p.model)
                            .desired_width(f32::INFINITY)
                            .hint_text(cat.t("ai.settings.model_hint")),
                    );
                } else {
                    let names: Vec<&str> = s.models.iter().map(String::as_str).collect();
                    let mut i = names.iter().position(|m| *m == p.model).unwrap_or(0);
                    if pick(ui, cx, "ddc.ai.model", &mut i, &names, 1.0) {
                        p.model = names[i].to_string();
                    }
                }
            });
            if p.auth != AuthKind::None {
                row(ui, cx, &cat.t("ai.settings.key"), |ui| {
                    ui.vertical(|ui| {
                        let hint = cat.t(if stored {
                            "ai.settings.key_stored"
                        } else {
                            "ai.settings.key_hint"
                        });
                        keys::secret_edit(ui, &mut s.key, key_field_id(), &hint);
                        ui.horizontal(|ui| {
                            keys::secret_actions(ui, th, cat, key_field_id(), &s.key);
                            if stored
                                && Button::quiet(&cat.t("ai.settings.key_remove"))
                                    .small()
                                    .show(ui, th)
                                    .clicked()
                            {
                                forget_key = true;
                            }
                        });
                        if let Some(k) = session_only_note(ddc_ai::secret::store_is_session_only())
                        {
                            kit::caption(ui, th, &cat.t(k));
                        }
                        if let Some(k) = held_note(held) {
                            kit::caption(ui, th, &cat.t(k));
                        }
                    });
                });
            }
        });
    egui::CollapsingHeader::new(
        egui::RichText::new(cat.t("ai.settings.advanced"))
            .font(th.font(FontRole::Label))
            .color(th.c("text.secondary")),
    )
    .id_salt(("ddc.ai.settings_advanced", p.id.as_str()))
    .default_open(open_advanced)
    .show(ui, |ui| {
        egui::Grid::new("ddc.ai.settings_advanced_form")
            .num_columns(2)
            .spacing([space::SM, space::XS])
            .striped(false)
            .show(ui, |ui| {
                row_tip(
                    ui,
                    cx,
                    &cat.t("ai.settings.protocol"),
                    Some(&cat.t("ai.settings.protocol_tip")),
                    |ui| {
                        let names = [
                            cat.t("ai.settings.protocol.chat"),
                            cat.t("ai.settings.protocol.blocks"),
                        ];
                        let mut i = usize::from(p.protocol == Protocol::ContentBlocks);
                        if pick(
                            ui,
                            cx,
                            "ddc.ai.protocol",
                            &mut i,
                            &[names[0].as_str(), names[1].as_str()],
                            1.0,
                        ) {
                            p.protocol = if i == 1 {
                                Protocol::ContentBlocks
                            } else {
                                Protocol::ChatCompletions
                            };
                        }
                    },
                );
                row(ui, cx, &cat.t("ai.settings.auth"), |ui| {
                    let names = [
                        cat.t("ai.settings.auth.none"),
                        cat.t("ai.settings.auth.bearer"),
                        cat.t("ai.settings.auth.header"),
                        cat.t("ai_safety.auth.query"),
                    ];
                    let mut i = match p.auth {
                        AuthKind::None => 0,
                        AuthKind::Bearer => 1,
                        AuthKind::Header { .. } => 2,
                        AuthKind::Query { .. } => 3,
                    };
                    let items: Vec<&str> = names.iter().map(String::as_str).collect();
                    if pick(ui, cx, "ddc.ai.auth", &mut i, &items, 1.0) {
                        p.auth = match i {
                            0 => AuthKind::None,
                            1 => AuthKind::Bearer,
                            2 => AuthKind::Header {
                                name: String::new(),
                            },
                            _ => AuthKind::Query {
                                name: String::new(),
                            },
                        };
                    }
                });
                if let AuthKind::Header { name } = &mut p.auth {
                    row(ui, cx, &cat.t("ai.settings.auth.header_name"), |ui| {
                        text_field(ui, name)
                    });
                }
                if let AuthKind::Query { name } = &mut p.auth {
                    row(ui, cx, &cat.t("ai_safety.auth.query_name"), |ui| {
                        text_field(ui, name)
                    });
                }
                row(ui, cx, &cat.t("ai.settings.headers"), |ui| {
                    ui.vertical(|ui| {
                        // while a key sits in the box it is masked, like the key field
                        let issue = keys::header_issue(&s.headers);
                        let hide = issue.is_some() && !keys::shown(ui.ctx(), headers_field_id());
                        let hint = cat.t("ai.settings.headers_hint");
                        keys::headers_edit(ui, &mut s.headers, headers_field_id(), &hint, hide);
                        if let Some((line, _)) = issue {
                            kit::error_note(ui, th, &cat.t("ai_keys.header_is_key"));
                            kit::caption(ui, th, &keys::masked_line(&s.headers, line));
                            ui.horizontal(|ui| {
                                let label = cat.t(if s.key.is_empty() {
                                    "ai_keys.move"
                                } else {
                                    "ai_keys.remove_line"
                                });
                                if Button::secondary(&label).small().show(ui, th).clicked() {
                                    move_key = true;
                                }
                                keys::show_toggle(ui, th, cat, headers_field_id());
                            });
                        }
                    });
                });
                row(ui, cx, &cat.t("ai.settings.tools"), |ui| {
                    let names = [
                        cat.t("ai.settings.tools.native"),
                        cat.t("ai.settings.tools.json"),
                    ];
                    let mut i = usize::from(p.tool_calling == ToolCalling::JsonInText);
                    if pick(
                        ui,
                        cx,
                        "ddc.ai.tools",
                        &mut i,
                        &[names[0].as_str(), names[1].as_str()],
                        1.0,
                    ) {
                        p.tool_calling = if i == 1 {
                            ToolCalling::JsonInText
                        } else {
                            ToolCalling::Native
                        };
                    }
                });
                row(ui, cx, &cat.t("ai.settings.images"), |ui| {
                    kit::checkbox(ui, th, &mut p.images, &cat.t("ai_keys.images_on"));
                });
                let ctx_salt = format!("ddc.ai.context_tokens.{}", p.id);
                row(ui, cx, &cat.t("ai.settings.context"), |ui| {
                    let limits = (1024, 4_000_000, 1024);
                    keys::token_field(ui, th, cat, &ctx_salt, &mut p.context_tokens, limits);
                });
                let out_salt = format!("ddc.ai.output_tokens.{}", p.id);
                row(ui, cx, &cat.t("ai.settings.output"), |ui| {
                    let limits = (256, 1_000_000, 256);
                    keys::token_field(ui, th, cat, &out_salt, &mut p.max_output_tokens, limits);
                });
                row_tip(
                    ui,
                    cx,
                    &cat.t("ai.settings.price_in"),
                    Some(&cat.t("ai.settings.price_tip")),
                    |ui| {
                        let err = parse_amount(&s.price_in, mark).err();
                        number_field(ui, cx, &mut s.price_in, "ai.settings.unit_price", err);
                    },
                );
                row_tip(
                    ui,
                    cx,
                    &cat.t("ai.settings.price_out"),
                    Some(&cat.t("ai.settings.price_tip")),
                    |ui| {
                        let err = parse_amount(&s.price_out, mark).err();
                        number_field(ui, cx, &mut s.price_out, "ai.settings.unit_price", err);
                    },
                );
                row_tip(
                    ui,
                    cx,
                    &cat.t("ai.settings.executor"),
                    Some(&cat.t("ai.settings.executor_tip")),
                    |ui| {
                        ui.add(
                            TextEdit::singleline(&mut s.exec_model)
                                .desired_width(f32::INFINITY)
                                .hint_text(cat.t("ai.settings.executor_hint")),
                        );
                    },
                );
                if !s.exec_model.trim().is_empty() {
                    row(ui, cx, &cat.t("ai.settings.executor_price_in"), |ui| {
                        let err = parse_amount(&s.exec_price_in, mark).err();
                        number_field(ui, cx, &mut s.exec_price_in, "ai.settings.unit_price", err);
                    });
                    row(ui, cx, &cat.t("ai.settings.executor_price_out"), |ui| {
                        let err = parse_amount(&s.exec_price_out, mark).err();
                        number_field(ui, cx, &mut s.exec_price_out, "ai.settings.unit_price", err);
                    });
                }
            });
    });
    if move_key {
        let m = keys::move_to_key(&s.headers, s.key.is_empty());
        s.headers = m.text;
        if let Some(k) = m.key {
            s.key = k;
        }
        if let Some(a) = m.auth {
            p.auth = a;
        }
        keys::forget(ctx, headers_field_id());
    }
    let id = p.id.clone();
    if forget_key {
        s.key_stored = connect::store().remove(&id).is_err();
        s.key.clear();
        forget_key_field(ctx);
    }
}

/// The presets as a title and a one-line explanation each; returns the one picked.
fn preset_list(ui: &mut Ui, cx: &Cx<'_>) -> Option<connect::Preset> {
    let (th, cat) = (cx.th, cx.cat);
    let mut picked = None;
    for preset in connect::Preset::ALL {
        let key = preset.key();
        if Button::secondary(&cat.t(key))
            .small()
            .full_width()
            .show(ui, th)
            .clicked()
        {
            picked = Some(preset);
        }
        kit::caption(ui, th, &cat.t(&format!("{key}_sub")));
        ui.add_space(space::XS);
    }
    picked
}

/// The "what to do" line under a failed test, when the catalog has one.
fn hint_of(cat: &ddc_ui::Catalog, key: &str) -> Option<String> {
    let k = format!("{key}.hint");
    let h = cat.t(&k);
    (h != k).then_some(h)
}

fn connections(
    ui: &mut Ui,
    s: &mut Settings,
    cx: &Cx<'_>,
    ctx: &egui::Context,
    acts: &mut Vec<Action>,
) {
    let (th, cat) = (cx.th, cx.cat);
    ui.horizontal(|ui| {
        if !s.draft.is_empty() {
            let names: Vec<String> = s.draft.iter().map(|p| p.name.clone()).collect();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let mut i = s.sel;
            if pick(ui, cx, "ddc.ai.settings_profile", &mut i, &refs, 0.5) {
                s.commit_form();
                s.sel = i;
                s.load_form();
            }
        }
        // With no connection yet the presets are in the body: one way in, not two.
        if !s.draft.is_empty() {
            // a kit button with a kit popup: the same look as the other buttons of the row
            let add = Button::secondary(&cat.t("ai.settings.add")).small().show(ui, th);
            egui::Popup::menu(&add).show(|ui| {
                if let Some(preset) = preset_list(ui, cx) {
                    s.add_preset(preset, cat);
                    ui.close();
                }
            });
        }
        if !s.draft.is_empty()
            && Button::danger(&cat.t("ai.settings.remove"))
                .small()
                .show(ui, th)
                .clicked()
        {
            let gone = s.draft.remove(s.sel);
            let _ = connect::store().remove(&gone.id);
            s.sel = s.sel.min(s.draft.len().saturating_sub(1));
            s.load_form();
        }
    });
    if s.draft.is_empty() {
        kit::empty_state(
            ui,
            th,
            &cat.t("ai.settings.no_profiles"),
            &cat.t("ai.settings.no_profiles_hint"),
            None,
        );
        if let Some(preset) = preset_list(ui, cx) {
            s.add_preset(preset, cat);
        }
        return;
    }
    ui.add_space(space::XS);
    form(ui, s, cx, ctx);
    if let Some(p) = s.draft.get(s.sel) {
        if !p.base_url.trim().is_empty() && !connect::is_local(p) {
            kit::caption(ui, th, &cat.t("ai.settings.cloud_note"));
        }
    }
    let missing = s.missing();
    for k in &missing {
        kit::caption(ui, th, &cat.t(k));
    }
    let busy = s.job.is_some();
    match &s.outcome {
        Some(Outcome::Ok(key)) => {
            ui.label(
                egui::RichText::new(cat.t(key))
                    .font(th.font(FontRole::Label))
                    .color(th.c("success")),
            );
        }
        Some(Outcome::Failed { key, detail }) => {
            kit::error_note(ui, th, &cat.t(key));
            if let Some(h) = hint_of(cat, key) {
                kit::caption(ui, th, &h);
            }
            if let Some(d) = detail {
                kit::caption(ui, th, d);
            }
        }
        None if busy => kit::loading_state(ui, th, &cat.t("ai.settings.testing")),
        None => {}
    }
    if let Some(e) = &s.save_error {
        kit::error_note(ui, th, e);
    }
    let blocked = s.save_blocked();
    let block_reason = blocked.map(|k| cat.t(k)).unwrap_or_default();
    if blocked.is_some() {
        kit::caption(ui, th, &block_reason);
    }
    kit::dialog_footer(ui, |ui| {
        let save_label = cat.t("ai.common.save");
        let mut save = Button::primary(&save_label).enabled(blocked.is_none());
        if blocked.is_some() {
            save = save.tooltip(&block_reason, "");
        }
        if save.show(ui, th).clicked() {
            s.save(ctx, cat, acts);
        }
        if Button::secondary(&cat.t("ai.settings.test"))
            .enabled(!busy && missing.is_empty())
            .tooltip(&cat.t("ai.settings.test_tip"), "")
            .show(ui, th)
            .clicked()
        {
            s.spawn(ctx, false);
        }
        if Button::quiet(&cat.t("ai.settings.list_models"))
            .enabled(!busy && !missing.contains(&"ai.settings.need_address"))
            .show(ui, th)
            .clicked()
        {
            s.spawn(ctx, true);
        }
    });
}

fn set_flag(m: &Model, key: &str, on: bool, acts: &mut Vec<Action>) {
    if m.set_ai_policy(key, if on { "1" } else { "0" }).is_ok() {
        acts.push(Action::RestartSession);
    }
}

/// A limit that is empty stays what it was; one that does not parse is refused with a note and
/// the old value stays.
#[allow(clippy::too_many_arguments)]
fn limit_field(
    ui: &mut Ui,
    cx: &Cx<'_>,
    label: &str,
    key: &str,
    unit_key: &str,
    value: &mut String,
    err: &mut Option<&'static str>,
    acts: &mut Vec<Action>,
) {
    row(ui, cx, label, |ui| {
        let r = number_field(ui, cx, value, unit_key, *err);
        if r.changed() {
            *err = None;
        }
        if r.lost_focus() {
            match commit_limit(&cx.ed.model, key, value, units::ui_decimal_mark()) {
                Ok(true) => {
                    *err = None;
                    acts.push(Action::RestartSession);
                }
                Ok(false) => *err = None,
                Err(k) => *err = Some(k),
            }
        }
    });
}

fn project(ui: &mut Ui, s: &mut Settings, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    let (th, cat, m) = (cx.th, cx.cat, &cx.ed.model);
    if kit::checkbox(ui, th, &mut s.local_only, &cat.t("ai.settings.local_only")).changed() {
        set_flag(m, "egress.local_only", s.local_only, acts);
    }
    kit::caption(ui, th, &cat.t("ai.settings.local_only_hint"));
    if kit::checkbox(ui, th, &mut s.snapshots, &cat.t("ai.consent.snapshots")).changed() {
        set_flag(m, "egress.snapshots", s.snapshots, acts);
    }
    kit::caption(ui, th, &cat.t("ai.consent.snapshots_hint"));
    if kit::checkbox(ui, th, &mut s.auto_snap, &cat.t("ai.settings.auto_snap")).changed() {
        set_flag(m, "ai.auto_snapshots", s.auto_snap, acts);
    }
    kit::caption(ui, th, &cat.t("ai.settings.auto_snap_hint"));
    if kit::checkbox(ui, th, &mut s.critic, &cat.t("ai.settings.critic")).changed() {
        set_flag(m, "ai.critic", s.critic, acts);
    }
    kit::caption(ui, th, &cat.t("ai.settings.critic_hint"));
    if kit::checkbox(ui, th, &mut s.refine, &cat.t("ai.settings.refine")).changed() {
        set_flag(m, crate::ai::project_tools::POLICY_REFINE, s.refine, acts);
    }
    kit::caption(ui, th, &cat.t("ai.settings.refine_hint"));
    if kit::checkbox(ui, th, &mut s.mep_auto, &cat.t("gen.mep_auto")).changed() {
        set_flag(m, crate::ai::mep_gate::POLICY_MEP_AUTO, s.mep_auto, acts);
    }
    kit::caption(ui, th, &cat.t("gen.mep_auto_hint"));
    if kit::checkbox(ui, th, &mut s.keep_text, &cat.t("ai.settings.keep_text")).changed() {
        set_flag(m, "audit.keep_text", s.keep_text, acts);
    }
    kit::section_title(ui, th, &cat.t("ai.settings.limits"));
    egui::Grid::new("ddc.ai.settings_limits")
        .num_columns(2)
        .spacing([space::SM, space::XS])
        .show(ui, |ui| {
            limit_field(
                ui,
                cx,
                &cat.t("ai.settings.max_cost"),
                "limits.max_cost",
                "ai.settings.unit_money",
                &mut s.max_cost,
                &mut s.limit_err[0],
                acts,
            );
            limit_field(
                ui,
                cx,
                &cat.t("ai.settings.max_tokens"),
                "limits.max_turn_tokens",
                "ai.settings.unit_tokens",
                &mut s.max_tokens,
                &mut s.limit_err[1],
                acts,
            );
        });
    kit::caption(ui, th, &cat.t("ai.settings.limits_hint"));
    kit::section_title(ui, th, &cat.t("ai.settings.consents"));
    if s.consents.is_empty() {
        kit::caption(ui, th, &cat.t("ai.settings.consents_none"));
    }
    let mut revoke = None;
    for host in &s.consents {
        ui.horizontal(|ui| {
            if Button::quiet(&cat.t("ai.settings.revoke"))
                .small()
                .show(ui, th)
                .clicked()
            {
                revoke = Some(host.clone());
            }
            kit::elided_label(ui, host, th.font(FontRole::Label), th.c("text.primary"));
        });
    }
    if let Some(host) = revoke {
        if m.set_ai_policy(&logic::consent_policy(&host), "0").is_ok() {
            s.consents = consents_of(m);
            acts.push(Action::RestartSession);
        }
    }
}

/// Agent access lives here: switch, port, token and the choice between asking and applying.
/// Both the panel's bar and this tab read the policy from the project, so they never disagree.
fn mcp(ui: &mut Ui, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    let (th, cat) = (cx.th, cx.cat);
    kit::section_title(ui, th, &cat.t("ai.mcp.access"));
    let out = crate::ai::mcp_bridge::access_ui(
        ui,
        th.c("warning"),
        crate::ai::session::live_policy(cx.ed),
    );
    acts.extend(logic::access_actions(out.renew, out.policy));
    ui.add_space(space::SM);
    mcp_server_button(ui, th, cat, acts);
}

fn mcp_server_button(ui: &mut egui::Ui, th: &ddc_ui::Theme, cat: &ddc_ui::Catalog, acts: &mut Vec<Action>) {
    if Button::secondary(&cat.t("mcp.set.title"))
        .small()
        .show(ui, th)
        .clicked()
    {
        acts.push(Action::McpServer);
    }
}

pub fn window(ctx: &egui::Context, st: &mut PanelUi, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    if !st.settings_open {
        if st.settings.loaded {
            st.settings.close(ctx);
        }
        return;
    }
    if !st.settings.loaded {
        let sel = st.profile;
        st.settings.load(&st.profiles, sel, &cx.ed.model);
    }
    st.settings.poll();
    let (th, cat) = (cx.th, cx.cat);
    let title = cat.t("cmd.ai.settings");
    let s = &mut st.settings;
    Dialog::new("ai.settings", &title, DialogSize::Tool)
        .open(&mut st.settings_open)
        .scroll_body()
        .show(ctx, |ui| {
            let tabs = [
                cat.t("ai.settings.tab_connections"),
                cat.t("ai.settings.tab_project"),
                cat.t("ai.settings.tab_mcp"),
            ];
            kit::tab_bar(
                ui,
                th,
                &[tabs[0].as_str(), tabs[1].as_str(), tabs[2].as_str()],
                &mut s.tab,
            );
            ui.add_space(space::SM);
            match s.tab {
                TAB_CONNECTIONS => connections(ui, s, cx, ctx, acts),
                TAB_PROJECT => project(ui, s, cx, acts),
                _ => mcp(ui, cx, acts),
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    const RU: DecimalMark = DecimalMark::Comma;
    const EN: DecimalMark = DecimalMark::Point;

    #[test]
    fn ru_comma_and_en_point_are_accepted() {
        assert_eq!(parse_amount("0,5", RU), Ok(Some(0.5)));
        assert_eq!(parse_amount(" 1 000,25 ", RU), Ok(Some(1000.25)));
        assert_eq!(parse_amount("0.5", EN), Ok(Some(0.5)));
        assert_eq!(parse_amount("0", EN), Ok(Some(0.0)), "zero means no limit");
        assert_eq!(parse_amount("  ", EN), Ok(None), "empty is not an error");
    }

    #[test]
    fn garbage_and_wrong_mark_are_refused() {
        assert_eq!(parse_amount("abc", EN), Err("ai.settings.num_bad"));
        assert_eq!(parse_amount("abc", RU), Err("ai.settings.num_bad"));
        assert_eq!(parse_amount("0,5", EN), Err("ai.settings.num_comma"));
        assert_eq!(parse_amount("-1", EN), Err("ai.settings.num_negative"));
        assert_eq!(parse_amount("-0,5", RU), Err("ai.settings.num_negative"));
    }

    #[test]
    fn limit_saved_in_stored_form_and_refusal_keeps_old_value() {
        let ed = crate::editor::Editor::new_in_memory().unwrap();
        let m = &ed.model;
        let read = || m.ai_policy("limits.max_cost").unwrap();
        assert_eq!(commit_limit(m, "limits.max_cost", "0,5", RU), Ok(true));
        assert_eq!(read().as_deref(), Some("0.5"));
        assert_eq!(commit_limit(m, "limits.max_cost", "abc", RU), Err("ai.settings.num_bad"));
        assert_eq!(commit_limit(m, "limits.max_cost", "0,7", EN), Err("ai.settings.num_comma"));
        assert_eq!(commit_limit(m, "limits.max_cost", "-2", EN), Err("ai.settings.num_negative"));
        assert_eq!(read().as_deref(), Some("0.5"), "old limit stays");
        assert_eq!(commit_limit(m, "limits.max_cost", "", EN), Ok(false));
        assert_eq!(read().as_deref(), Some("0.5"));
        assert_eq!(commit_limit(m, "limits.max_cost", "1.25", EN), Ok(true));
        assert_eq!(read().as_deref(), Some("1.25"));
    }

    #[test]
    fn price_round_trip_in_ru() {
        let shown = price_text(Some(0.15), RU);
        assert_eq!(shown, "0,15");
        assert_eq!(parse_amount(&shown, RU), Ok(Some(0.15)));
        assert_eq!(price_text(Some(0.15), EN), "0.15");
        assert_eq!(price_text(None, RU), "");
        assert_eq!(stored_text(Some("0.5".into()), RU), "0,5");
        assert_eq!(stored_text(Some("0.5".into()), EN), "0.5");
        assert_eq!(stored_text(None, RU), "");
    }

    #[test]
    fn bad_price_blocks_save_and_keeps_the_old_price() {
        let mut s = Settings::default();
        s.price_in = amount_text(0.5, units::ui_decimal_mark());
        assert_eq!(s.save_blocked(), None);
        s.price_out = "abc".into();
        assert_eq!(s.save_blocked(), Some("ai.settings.num_bad"));
        let mut p = connect::preset_profile(connect::Preset::Custom, "p1".into(), "P".into());
        p.price_out_per_mtok = Some(2.0);
        s.draft = vec![p];
        s.commit_form();
        assert_eq!(s.draft[0].price_out_per_mtok, Some(2.0), "refused text does not drop the price");
        assert_eq!(s.price_out, "abc", "typed text stays");
    }

    #[test]
    fn ai_panel_spacing_uses_tokens() {
        for (name, src) in [
            ("chat.rs", include_str!("chat.rs")),
            ("md.rs", include_str!("md.rs")),
            ("mod.rs", include_str!("mod.rs")),
        ] {
            for pat in ["Margin::same(", "Margin::symmetric("] {
                for part in src.split(pat).skip(1) {
                    let next = part.chars().next().unwrap_or(' ');
                    assert!(!next.is_ascii_digit(), "{name}: literal after {pat}");
                }
            }
        }
    }

    #[test]
    fn new_strings_exist_in_both_languages() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            for k in [
                "ai.settings.num_bad",
                "ai.settings.num_comma",
                "ai.settings.num_negative",
                "ai.settings.unit_money",
                "ai.settings.unit_tokens",
                "ai.settings.unit_price",
                "ai.settings.limits_hint",
            ] {
                assert_ne!(cat.t(k), k, "{k}");
            }
        }
    }

    #[test]
    fn mcp_tab_offers_server_settings() {
        let ed = crate::editor::Editor::new_in_memory().unwrap();
        let th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            assert_ne!(cat.t("mcp.set.title"), "mcp.set.title", "label exists");
            let cx = Cx {
                th: &th,
                cat: &cat,
                ed: &ed,
                view3d: false,
                palette_open: false,
            };
            let mut acts = Vec::new();
            // the widgets draw with the bundled named weights
            let ctx = egui::Context::default();
            ctx.set_fonts(ddc_ui::theme::font_definitions());
            ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| mcp(ui, &cx, &mut acts));
            });
            assert!(acts.is_empty(), "rendering alone pushes nothing");
        }
    }

    #[test]
    fn mcp_server_button_click_dispatches_action() {
        let th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let cat = ddc_ui::Catalog::new(ddc_ui::Lang::En);
        let ctx = egui::Context::default();
        ctx.set_fonts(ddc_ui::theme::font_definitions());
        let mut acts = Vec::new();
        let mut centre = egui::Pos2::ZERO;
        let mut frame = |ev: Vec<egui::Event>, acts: &mut Vec<Action>, centre: &mut egui::Pos2| {
            let input = egui::RawInput { events: ev, ..Default::default() };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    // the panel's own rect fills the screen; the scope's rect is the button's
                    let r = ui.scope(|ui| mcp_server_button(ui, &th, &cat, acts)).response.rect;
                    *centre = r.center();
                });
            });
        };
        frame(vec![], &mut acts, &mut centre);
        frame(vec![egui::Event::PointerMoved(centre)], &mut acts, &mut centre);
        let btn = |pressed, pos| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![btn(true, centre)], &mut acts, &mut centre);
        assert!(acts.is_empty(), "press alone is not a click");
        frame(vec![btn(false, centre)], &mut acts, &mut centre);
        assert!(
            acts.iter().any(|a| matches!(a, Action::McpServer)),
            "click dispatches Action::McpServer"
        );
    }

    fn failed_of(e: ddc_ai::ProviderError) -> Option<Settled> {
        settle(Ok(failed(&e)))
    }

    fn failure(s: Option<Settled>) -> (&'static str, Option<String>) {
        match s {
            Some(Settled::Done(Outcome::Failed { key, detail })) => (key, detail),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn settings_ping_ok_and_models_show_outcome() {
        assert_eq!(
            settle(Ok(JobMsg::Tested { tool: true })),
            Some(Settled::Done(Outcome::Ok("ai.settings.test_ok")))
        );
        assert_eq!(
            settle(Ok(JobMsg::Tested { tool: false })),
            Some(Settled::Done(Outcome::Ok("ai.settings.test_no_tool")))
        );
        assert_eq!(
            settle(Ok(JobMsg::Models(vec!["m".into()]))),
            Some(Settled::Models(
                vec!["m".into()],
                Outcome::Ok("ai.settings.models_ok")
            ))
        );
        assert_eq!(settle(Err(mpsc::TryRecvError::Empty)), None);
    }

    #[test]
    fn settings_http_401_shows_status() {
        let (key, detail) = failure(failed_of(ddc_ai::ProviderError::Http {
            status: 401,
            body: "invalid api key".into(),
        }));
        assert_eq!(key, "ai.err.server");
        let d = detail.expect("detail");
        assert!(d.contains("401") && d.contains("invalid"), "{d}");
    }

    #[test]
    fn settings_429_shows_retry_after() {
        let (key, detail) = failure(failed_of(ddc_ai::ProviderError::RateLimited {
            retry_after_s: Some(12),
        }));
        assert_eq!(key, "ai.err.rate_limit");
        assert!(detail.unwrap().contains("retry after 12 s"));
    }

    #[test]
    fn settings_job_panic_shows_error() {
        let (tx, rx) = mpsc::channel::<JobMsg>();
        let h = std::thread::spawn(move || {
            let _tx = tx;
            panic!("job died");
        });
        let _ = h.join();
        let (key, detail) = failure(settle(rx.try_recv()));
        assert_eq!(key, "ai.settings.job_crashed");
        assert_eq!(detail, None);
    }

    #[test]
    fn settings_spawn_failure_shows_error() {
        assert_eq!(
            spawn_failed(),
            Outcome::Failed {
                key: "ai.settings.job_spawn_failed",
                detail: None
            }
        );
    }

    #[test]
    fn settings_ping_401_shows_detail_without_key() {
        let _typed = Secret::new("typed-key-9f8e7d6c5b4a");
        let (_, detail) = failure(failed_of(ddc_ai::ProviderError::Http {
            status: 401,
            body: "bad key typed-key-9f8e7d6c5b4a and sk-abcDEF1234567890xyz".into(),
        }));
        let d = detail.unwrap();
        assert!(d.contains("401"), "{d}");
        assert!(!d.contains("typed-key-9f8e7d6c5b4a"), "{d}");
        assert!(!d.contains("sk-abcDEF"), "{d}");
    }

    #[test]
    fn outcome_keys_exist_in_both_languages() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            for k in [
                "ai.settings.job_crashed",
                "ai.settings.job_spawn_failed",
                "ai.settings.models_ok",
            ] {
                assert_ne!(cat.t(k), k, "{k}");
            }
        }
    }

    /// "Model" alone means the BIM model; the engine is always "AI model" (ru: the stem + the Russian abbreviation for AI).
    #[test]
    fn ai_texts_do_not_call_the_engine_model() {
        const ENGINE_KEYS: [&str; 26] = [
            "ai.err.context",
            "ai.err.limit_filtered",
            "ai.err.limit_output",
            "ai.err.model",
            "ai.err.model.hint",
            "ai.err.no_tools",
            "ai.err.no_tools.hint",
            "ai.err.server.hint",
            "ai.settings.images",
            "ai.settings.list_models",
            "ai.settings.model",
            "ai.settings.model_hint",
            "ai.settings.models_ok",
            "ai.settings.need_model",
            "ai.settings.test_no_tool",
            "ai.settings.test_ok",
            "ai.settings.no_profiles_hint",
            "ai.state.error",
            "ai.state.local_only.hint",
            "ai.state.no_profile",
            "ai.state.no_profile.action",
            "ai.state.no_profile.hint",
            "cmd.ai.settings.desc",
            "cmd.ai.what_sent.desc",
            "ai.hist.tip",
            "ai.settings.preset.local_sub",
        ];
        let words = |s: &str| -> Vec<String> {
            s.split_whitespace()
                // placeholders like {model} are values, not wording
                .filter(|w| !w.contains('{'))
                .map(|w| {
                    w.trim_matches(|c: char| !c.is_alphanumeric())
                        .to_lowercase()
                })
                .collect()
        };
        let en = ddc_ui::Catalog::new(ddc_ui::Lang::En);
        let ru = ddc_ui::Catalog::new(ddc_ui::Lang::Ru);
        let ru_stem = "\u{43c}\u{43e}\u{434}\u{435}\u{43b}";
        let ru_ai = "\u{438}\u{438}";
        for k in ENGINE_KEYS {
            let (e, r) = (en.t(k), ru.t(k));
            assert_ne!(e, k, "{k} en");
            assert_ne!(r, k, "{k} ru");
            let w = words(&e);
            for (i, x) in w.iter().enumerate() {
                if x.starts_with("model") {
                    assert!(i > 0 && w[i - 1] == "ai", "{k} en: {e}");
                }
            }
            let w = words(&r);
            for (i, x) in w.iter().enumerate() {
                if x.starts_with(ru_stem) {
                    assert!(w.get(i + 1).is_some_and(|n| n == ru_ai), "{k} ru: {r}");
                }
            }
        }
        assert!(!en.t("ai.state.no_profile").to_lowercase().contains("model"));
        assert!(!en.t("ai.state.error").to_lowercase().contains("did not work"));
    }

    #[test]
    fn prices_round_trip_as_text() {
        assert_eq!(price_text(None, DecimalMark::Point), "");
        assert_eq!(price_text(Some(1.5), DecimalMark::Point), "1.5");
    }

    #[test]
    fn consents_list_only_what_is_granted() {
        let m = Model::open(":memory:").unwrap();
        m.set_ai_policy("egress.consent.https://a.test", "1")
            .unwrap();
        m.set_ai_policy("egress.consent.https://b.test", "0")
            .unwrap();
        m.set_ai_policy("egress.snapshots", "1").unwrap();
        assert_eq!(consents_of(&m), vec!["https://a.test".to_string()]);
    }

    const FAKE_KEY: &str = "fake-key-SECRETSECRET-77";

    struct RefusingStore;

    impl SecretStore for RefusingStore {
        fn get(&self, _: &str) -> Result<Option<Secret>, ddc_ai::SecretError> {
            Ok(None)
        }
        fn set(&self, _: &str, _: Secret) -> Result<(), ddc_ai::SecretError> {
            Err(ddc_ai::SecretError::Store("refused".into()))
        }
        fn remove(&self, _: &str) -> Result<(), ddc_ai::SecretError> {
            Ok(())
        }
    }

    fn one_profile() -> Settings {
        let mut s = Settings::default();
        let mut p = connect::blank_profile("p1".into(), "x".into());
        p.base_url = "https://h.test/v1".into();
        p.auth = AuthKind::Bearer;
        s.draft = vec![p];
        s
    }

    #[test]
    fn the_executor_model_is_a_setting_of_the_connection_and_empty_means_none() {
        let mut s = one_profile();
        assert!(s.draft[0].executor.is_none(), "default: one model for every step");
        s.exec_model = " small ".into();
        s.exec_price_in = "2".into();
        s.commit_form();
        let e = s.draft[0].executor.clone().expect("an executor");
        assert_eq!(e.model, "small");
        assert_eq!(e.price_in_per_mtok, Some(2.0));
        assert_eq!(e.price_out_per_mtok, None);
        // a refused price keeps the old one and blocks Save, like the main prices
        s.exec_price_in = "abc".into();
        s.commit_form();
        assert_eq!(s.draft[0].executor.as_ref().unwrap().price_in_per_mtok, Some(2.0));
        assert!(s.save_blocked().is_some());
        s.exec_model.clear();
        s.commit_form();
        assert!(s.draft[0].executor.is_none());
    }

    #[test]
    fn a_refused_key_stays_in_the_field_and_nothing_is_written() {
        let mut s = one_profile();
        s.key = FAKE_KEY.into();
        let mut wrote = false;
        let r = s.save_with(&RefusingStore, |_| {
            wrote = true;
            Ok(())
        });
        assert!(matches!(r, Err(SaveErr::Key)));
        assert_eq!(s.key, FAKE_KEY, "the typed key is not lost");
        assert!(!wrote, "the connection is not saved without its key");
        assert!(!s.key_stored);
    }

    #[test]
    fn a_held_address_key_stays_until_stored_and_is_never_written() {
        let mut s = one_profile();
        s.draft[0].base_url = format!("https://h.test/v1?key={FAKE_KEY}");
        let mut other = connect::blank_profile("p2".into(), "y".into());
        other.base_url = "https://o.test/v1".into();
        s.draft.push(other);
        s.held = take_held(&mut s.draft);
        assert_eq!(s.draft[0].base_url, "https://h.test/v1");
        assert_eq!(s.draft[0].auth, AuthKind::Query { name: "key".into() });
        assert_eq!(s.held, vec![("p1".to_string(), FAKE_KEY.to_string())]);
        // saving another connection while the store refuses: nothing written, nothing lost
        s.sel = 1;
        let mut wrote = false;
        let r = s.save_with(&RefusingStore, |_| {
            wrote = true;
            Ok(())
        });
        assert!(matches!(r, Err(SaveErr::Key)) && !wrote);
        assert_eq!(s.held.len(), 1);
        // the held key follows its field; once stored, the list is written without it
        s.sel = 0;
        s.key = FAKE_KEY.into();
        s.commit_form();
        assert_eq!(s.held.len(), 1);
        let store = ddc_ai::MemoryStore::new();
        let mut written = String::new();
        let r = s.save_with(&store, |list| {
            written = format!("{list:?}");
            Ok(())
        });
        assert!(r.is_ok());
        assert_eq!(store.get("p1").unwrap().unwrap(), Secret::new(FAKE_KEY));
        assert!(!written.contains(FAKE_KEY), "{written}");
        assert!(s.held.is_empty() && s.key.is_empty());
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            for k in [held_note(true).unwrap(), "ai.settings.key_store_failed"] {
                let t = cat.t(k);
                assert!(t != k && !t.contains(FAKE_KEY), "{k}");
            }
        }
        assert_eq!(held_note(false), None);
    }

    #[test]
    fn a_saved_key_leaves_the_field_only_after_both_writes() {
        let store = ddc_ai::MemoryStore::new();
        let mut s = one_profile();
        s.key = FAKE_KEY.into();
        let r = s.save_with(&store, |_| Err("disk full".into()));
        assert!(matches!(r, Err(SaveErr::Profiles(_))));
        assert_eq!(s.key, FAKE_KEY);
        assert!(s.save_with(&store, |_| Ok(())).is_ok());
        assert!(s.key.is_empty() && s.key_stored);
        assert!(store.get("p1").unwrap().is_some());
    }

    #[test]
    fn a_key_typed_into_the_address_moves_to_the_key_field() {
        let mut s = one_profile();
        s.draft[0].auth = AuthKind::None;
        s.draft[0].base_url = format!("https://h.test/v1?key={FAKE_KEY}");
        s.commit_form();
        assert_eq!(s.draft[0].base_url, "https://h.test/v1");
        assert_eq!(s.draft[0].auth, AuthKind::Query { name: "key".into() });
        assert_eq!(s.key, FAKE_KEY);
        let d = std::env::temp_dir().join(format!("ddc-ai18-{}", std::process::id()));
        let mut written = String::new();
        let r = s.save_with(&ddc_ai::MemoryStore::new(), |list| {
            ddc_ai::profile::save_profiles(&d, list).map_err(|e| e.to_string())?;
            written = std::fs::read_to_string(ddc_ai::profile::profiles_path(&d)).unwrap();
            Ok(())
        });
        assert!(r.is_ok());
        assert!(!written.is_empty() && !written.contains(FAKE_KEY), "{written}");
        // a typed key wins over the one in the address
        let mut p = s.draft[0].clone();
        p.base_url = "https://h.test/v1?token=other-SECRET-1".into();
        let mut key = FAKE_KEY.to_string();
        lift_url_key(&mut p, &mut key);
        assert_eq!(key, FAKE_KEY);
        assert!(!p.base_url.contains("other-SECRET-1"));
    }

    #[test]
    fn a_session_only_store_says_so_in_both_languages() {
        assert_eq!(session_only_note(false), None);
        let k = session_only_note(true).unwrap();
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            for k in [
                k,
                "ai_safety.auth.query",
                "ai_safety.auth.query_name",
                "ai_safety.need_query_name",
                "ai.settings.no_data_dir",
            ] {
                assert_ne!(cat.t(k), k, "{k}");
            }
        }
    }
}
