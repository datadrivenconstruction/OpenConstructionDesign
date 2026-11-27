// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Profiles, the key store and the gate in front of the first request. A provider is built once
//! per session with its egress guard, so a consent or profile change means a new session.

use super::logic;
use crate::ai::{AiError, SessionCfg, UiSession, ViewCtx};
use crate::shell::EditorApp;
use ddc_ai::egress::is_local_url;
use ddc_ai::profile::header_is_secret;
use ddc_ai::secret::default_store;
use ddc_ai::{
    build_provider, consent_key, AuthKind, EgressGuard, Limits, Profile, Protocol, SecretStore,
    ToolCalling,
};
use ddc_model::Model;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};
use web_time::{SystemTime, UNIX_EPOCH};

/// The one store of the process: on systems without a key store it is memory, a new one per call.
pub fn store() -> &'static Arc<dyn SecretStore> {
    static STORE: OnceLock<Arc<dyn SecretStore>> = OnceLock::new();
    STORE.get_or_init(default_store)
}

/// The list, and what happened to a list file that could not be read (for a notice).
pub fn load_profiles() -> (Vec<Profile>, Option<Unreadable>) {
    crate::gfx::data_dir()
        .map(|d| load_profiles_noting(&d, &**store()))
        .unwrap_or_default()
}

/// A list file that could not be read: its masked copy in `_archive`, or why it stayed.
#[derive(Debug)]
pub enum Unreadable {
    Archived(ddc_ai::profile::Damaged),
    Stayed(String),
}

/// The en/ru notice for an unreadable list file; it says where the file is and that it may
/// still hold a key.
pub fn unreadable_note(cat: &ddc_ui::Catalog, u: &Unreadable) -> String {
    match u {
        Unreadable::Archived(d) => {
            let key = if d.scrubbed {
                "ai.settings.profiles_archived"
            } else {
                "ai.settings.profiles_archived_raw"
            };
            cat.tf(key, &[("path", &d.path.display().to_string())])
        }
        Unreadable::Stayed(why) => cat.tf("ai.settings.profiles_unreadable", &[("reason", why)]),
    }
}

/// As `load_profiles_in`; a file that is not a list goes to `_archive` with its keys masked
/// (`ddc_ai::profile::archive_damaged`) and the list starts empty.
pub fn load_profiles_noting(
    dir: &std::path::Path,
    keys: &dyn SecretStore,
) -> (Vec<Profile>, Option<Unreadable>) {
    match ddc_ai::profile::load_profiles(dir) {
        Ok(list) => (clean_loaded(dir, keys, list), None),
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            let what = match ddc_ai::profile::archive_damaged(dir) {
                Ok(d) => Unreadable::Archived(d),
                Err(e) => Unreadable::Stayed(ddc_ai::secret::scrub_known(&e.to_string())),
            };
            (Vec::new(), Some(what))
        }
        Err(_) => (Vec::new(), None),
    }
}

/// The list in `dir`, with a key left in an address by an older version moved into `keys`
/// and the file rewritten without it. A key already in the store wins over the address one.
/// A key the store refuses stays in its address in memory only, where the settings form takes
/// it from; the file is still rewritten without it, so it is asked for again after a restart.
/// A credential header (by name or by value) is never written back either (see
/// `lift_secret_header`).
pub fn load_profiles_in(dir: &std::path::Path, keys: &dyn SecretStore) -> Vec<Profile> {
    load_profiles_noting(dir, keys).0
}

fn clean_loaded(
    dir: &std::path::Path,
    keys: &dyn SecretStore,
    mut list: Vec<Profile>,
) -> Vec<Profile> {
    let (mut lifted, mut held) = (false, 0usize);
    for p in &mut list {
        if ddc_ai::profile::split_url_secret(&p.base_url).is_none() {
            continue;
        }
        lifted = true;
        let mut clean = p.clone();
        let Some(k) = ddc_ai::profile::lift_url_secret(&mut clean) else {
            *p = clean;
            continue;
        };
        match keys.get(&p.id) {
            // the stored key is the one sent, in the mode the profile already had
            Ok(Some(_)) => {
                if p.auth != AuthKind::None {
                    clean.auth = p.auth.clone();
                }
                *p = clean;
            }
            Ok(None) if keys.set(&p.id, k).is_ok() => *p = clean,
            _ => held += 1,
        }
    }
    for p in &mut list {
        if !p.extra_headers.iter().any(|(k, v)| header_is_secret(k, v)) {
            continue;
        }
        lifted = true;
        held += lift_secret_header(p, keys);
    }
    if !lifted {
        return list;
    }
    // the disk copy loses every address key and credential header, held ones too; the temp
    // file is renamed over the old one, so no copy with a key is left behind
    let disk: Vec<Profile> = list
        .iter()
        .map(|p| {
            let mut c = p.clone();
            drop(ddc_ai::profile::lift_url_secret(&mut c));
            c.extra_headers.retain(|(k, v)| !header_is_secret(k, v));
            c
        })
        .collect();
    if let Err(e) = ddc_ai::profile::save_profiles(dir, &disk) {
        eprintln!("assistant profiles: {}", ddc_ai::secret::scrub_known(&e.to_string()));
    }
    if held > 0 {
        eprintln!("assistant profiles: {held} key(s) not stored; kept until the app closes");
    }
    list
}

/// A credential header left in the list by an older version becomes the profile key (sent as
/// the same header) when the profile has no stored key and the store takes it. Any other stays in
/// memory only, masked in the headers box that blocks a save. Returns how many stay there.
fn lift_secret_header(p: &mut Profile, keys: &dyn SecretStore) -> usize {
    // without a stored key the profile could only have worked through this header
    if matches!(keys.get(&p.id), Ok(None)) {
        let i = p.extra_headers.iter().position(|(k, v)| header_is_secret(k, v));
        if let Some(i) = i.filter(|&i| !p.extra_headers[i].1.trim().is_empty()) {
            let value = ddc_ai::Secret::new(p.extra_headers[i].1.trim());
            if keys.set(&p.id, value).is_ok() {
                let (name, _) = p.extra_headers.remove(i);
                p.auth = AuthKind::Header { name };
            }
        }
    }
    p.extra_headers.iter().filter(|(k, v)| header_is_secret(k, v)).count()
}

pub fn save_profiles(list: &[Profile]) -> Result<(), String> {
    save_profiles_in(crate::gfx::data_dir(), list)
}

/// `dir` is `None` when the system gives no folder for app data: the reason is said.
pub fn save_profiles_in(dir: Option<std::path::PathBuf>, list: &[Profile]) -> Result<(), String> {
    let dir = dir.ok_or_else(|| crate::tr("ai.settings.no_data_dir", &[]))?;
    ddc_ai::profile::save_profiles(&dir, list).map_err(|e| e.to_string())
}

pub fn blank_profile(id: String, name: String) -> Profile {
    ddc_ai::profile::blank(id, name)
}

/// Where a new connection starts, so a first-time user never faces an empty form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// A server on this computer: address filled, no key.
    Local,
    /// A hosted service: needs its address and a key.
    Hosted,
    /// A model gateway speaking chat completions: one key, many models; address filled, models and prices
    /// come from its model list.
    Gateway,
    /// Every field by hand, the advanced section open.
    Custom,
}

impl Preset {
    pub const ALL: [Preset; 4] = [Preset::Local, Preset::Gateway, Preset::Hosted, Preset::Custom];

    /// Menu title key; the subtitle key is this plus `_sub`.
    pub const fn key(self) -> &'static str {
        match self {
            Preset::Local => "ai.settings.preset.local",
            Preset::Hosted => "ai.settings.preset.cloud",
            Preset::Gateway => "ai.settings.preset.gateway",
            Preset::Custom => "ai.settings.preset.custom",
        }
    }
}

/// The usual address of a local server that speaks chat messages.
pub const LOCAL_URL: &str = "http://localhost:11434/v1";

pub fn preset_profile(preset: Preset, id: String, name: String) -> Profile {
    let mut p = blank_profile(id, name);
    match preset {
        Preset::Local => {
            p.base_url = LOCAL_URL.to_string();
            p.auth = AuthKind::None;
            p.protocol = Protocol::ChatCompletions;
        }
        Preset::Hosted => p.auth = AuthKind::Bearer,
        Preset::Gateway => {
            p.base_url = ddc_ai::gateway::GATEWAY_URL.to_string();
            p.auth = AuthKind::Bearer;
            p.protocol = Protocol::ChatCompletions;
            p.tool_calling = ToolCalling::Native;
        }
        Preset::Custom => {}
    }
    p
}

/// What still stops this connection from working, as catalog keys, in form order.
/// `key_ready` is a key typed now or one already in the store.
pub fn missing(p: &Profile, key_ready: bool) -> Vec<&'static str> {
    let mut out = vec![];
    let url = p.base_url.trim();
    if url.is_empty() {
        out.push("ai.settings.need_address");
    } else if !(url.starts_with("http://") || url.starts_with("https://")) {
        out.push("ai.settings.bad_address");
    }
    if p.model.trim().is_empty() {
        out.push("ai.settings.need_model");
    }
    if p.auth != AuthKind::None && !key_ready && !is_local(p) {
        out.push("ai.settings.need_key");
    }
    if matches!(&p.auth, AuthKind::Header { name } if name.trim().is_empty()) {
        out.push("ai.settings.need_header_name");
    }
    if matches!(&p.auth, AuthKind::Query { name } if name.trim().is_empty()) {
        out.push("ai_safety.need_query_name");
    }
    out
}

/// The browser build reaches the model through the host page (`ddc-web-bridge/1`): the page
/// holds address and key, the editor neither. Its profile carries this address.
pub const PAGE_URL: &str = "page://host";

const ON_WEB: bool = cfg!(all(feature = "web", target_arch = "wasm32"));

/// The profile of the host page; the browser build's default when none is set up.
pub fn page_profile() -> Profile {
    let mut p = blank_profile("page".into(), "Host page".into());
    p.base_url = PAGE_URL.into();
    p.model = "host".into();
    p.auth = AuthKind::None;
    p
}

/// What the panel uses with no profile chosen: the host page in the browser, nothing elsewhere.
pub fn default_profile() -> Option<Profile> {
    ON_WEB.then(page_profile)
}

/// Only the browser build has a host page: elsewhere this address is an ordinary (bad) one.
fn uses_page(p: &Profile, on_web: bool) -> bool {
    on_web && p.base_url == PAGE_URL
}

#[cfg(all(feature = "web", target_arch = "wasm32"))]
fn page_provider() -> Result<Arc<dyn ddc_ai::Provider>, AiError> {
    Ok(Arc::new(ddc_ai::host_bridge::HostProvider::new(crate::web::ai_mailbox())))
}

#[cfg(not(all(feature = "web", target_arch = "wasm32")))]
fn page_provider() -> Result<Arc<dyn ddc_ai::Provider>, AiError> {
    Err(AiError::Provider("the host page is reached only in the browser build".into()))
}

pub fn is_local(p: &Profile) -> bool {
    is_local_url(&p.base_url)
}

fn policy(m: &Model, k: &str) -> Option<String> {
    m.ai_policy(k).ok().flatten()
}

pub fn policy_flag(m: &Model, k: &str, default: bool) -> bool {
    logic::policy_on(policy(m, k).as_deref(), default)
}

/// The project's own answer on images; `None` while it has not said.
fn snapshots_choice(m: &Model) -> Option<bool> {
    policy(m, "egress.snapshots").map(|v| logic::policy_on(Some(&v), false))
}

/// Images of the view leave the editor only when the project said so. Unanswered, the answer
/// follows where the provider is: a local one keeps them on this machine, a remote one gets none.
pub fn snapshots_for(m: &Model, p: Option<&Profile>) -> bool {
    snapshots_choice(m).unwrap_or_else(|| p.is_some_and(is_local))
}

/// What the consent card's snapshot box starts as: the saved answer, else ticked, because the
/// card spells out what goes out and the person can untick it; a model that cannot see its own
/// drawing cannot check it.
pub fn snapshots_card_default(m: &Model) -> bool {
    snapshots_choice(m).unwrap_or(true)
}

/// Saved answer only (settings form); see `snapshots_for` for what a session gets.
pub fn snapshots_on(m: &Model) -> bool {
    snapshots_choice(m).unwrap_or(false)
}

fn limits_of(m: &Model) -> Limits {
    let mut l = Limits::default();
    if let Some(v) = policy(m, "limits.max_cost").and_then(|v| v.parse().ok()) {
        l.max_cost = Some(v);
    }
    if let Some(v) = policy(m, "limits.max_turn_tokens").and_then(|v| v.parse().ok()) {
        l.max_turn_tokens = v;
    }
    l
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    NoProfile,
    /// This project is local only and the address is not.
    LocalBlocked,
    /// Nothing leaves until the person answers the card; the full address goes on it.
    NeedConsent {
        host: String,
    },
    BadAddress(String),
    /// `host` is what the guard lets through; a local address needs nothing.
    Ready {
        host: Option<String>,
    },
}

/// `once` is the address answered "once" in this session.
pub fn gate(m: &Model, p: Option<&Profile>, once: Option<&str>) -> Gate {
    gate_on(m, p, once, ON_WEB)
}

fn gate_on(m: &Model, p: Option<&Profile>, once: Option<&str>, on_web: bool) -> Gate {
    let Some(p) = p else {
        return Gate::NoProfile;
    };
    // the page is this editor's own frame: nothing leaves from here, the page owns egress
    if uses_page(p, on_web) {
        return Gate::Ready { host: None };
    }
    if p.base_url.trim().is_empty() || p.model.trim().is_empty() {
        return Gate::NoProfile;
    }
    if is_local(p) {
        return Gate::Ready { host: None };
    }
    let host = match consent_key(&p.base_url) {
        Ok(h) => h,
        Err(e) => return Gate::BadAddress(e.to_string()),
    };
    if policy_flag(m, "egress.local_only", false) {
        return Gate::LocalBlocked;
    }
    if once == Some(host.as_str()) || policy_flag(m, &logic::consent_policy(&host), false) {
        return Gate::Ready { host: Some(host) };
    }
    Gate::NeedConsent { host }
}

fn session_name() -> String {
    let s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format!("ui-{s}")
}

/// The key is read here and goes straight into the provider: it is never kept in the panel.
pub fn open_session(
    app: &EditorApp,
    p: &Profile,
    host: Option<String>,
    snapshots: Arc<AtomicBool>,
    view: ViewCtx,
) -> Result<UiSession, AiError> {
    let m = &app.ed.model;
    let provider = if uses_page(p, ON_WEB) {
        page_provider()?
    } else {
        direct_provider(m, p, host)?
    };
    let cfg = SessionCfg {
        provider,
        profile: p.clone(),
        limits: limits_of(m),
        session: session_name(),
        allow_snapshots: snapshots,
        extra_generators: vec![],
        extra_rules: vec![],
        keep_text: policy_flag(m, "audit.keep_text", false),
        confirm_ttl: crate::ai::session::CONFIRM_TTL,
        target: ddc_ai::catalog::Target::Main,
        auto_replan: true,
    };
    UiSession::start(&app.ed, cfg, view)
}

/// What one step may cost and the money left on the connection, asked off the UI thread once
/// per session (the same estimate `ddc ai` prints). Never asked of the host page.
pub fn balance_job(
    app: &EditorApp,
    p: &Profile,
    host: Option<String>,
) -> Option<std::sync::mpsc::Receiver<crate::ai::headless::Estimate>> {
    if uses_page(p, ON_WEB) || is_local(p) {
        return None;
    }
    let provider = direct_provider(&app.ed.model, p, host).ok()?;
    Some(spawn_estimate(p.clone(), provider))
}

pub fn spawn_estimate(
    p: Profile,
    provider: Arc<dyn ddc_ai::Provider>,
) -> std::sync::mpsc::Receiver<crate::ai::headless::Estimate> {
    let (tx, rx) = std::sync::mpsc::channel();
    let started = crate::jobs::spawn_guarded(
        "ai-balance",
        move || {
            let _ = tx.send(crate::ai::headless::estimate(&p, &*provider));
        },
        |_| {},
    );
    drop(started);
    rx
}

fn direct_provider(
    m: &Model,
    p: &Profile,
    host: Option<String>,
) -> Result<Arc<dyn ddc_ai::Provider>, AiError> {
    let key = if p.auth == AuthKind::None {
        None
    } else {
        store()
            .get(&p.id)
            .map_err(|e| AiError::Internal(e.to_string()))?
    };
    let guard = EgressGuard {
        local_only: policy_flag(m, "egress.local_only", false),
        consented_host: host,
    };
    build_provider(p, key, guard).map_err(|e| AiError::Provider(e.to_string()))
}

/// What the next turn tells the model about where the person is.
pub fn view_ctx(app: &EditorApp, with_selection: bool) -> ViewCtx {
    ViewCtx {
        view: if app.view3d { "3d" } else { "plan" }.to_string(),
        level: app.ed.level,
        units: app.ed.tool.units,
        selection: if with_selection {
            app.ed.selection.clone()
        } else {
            vec![]
        },
        bounds: None,
        ..ViewCtx::default()
    }
}

/// What the person has picked, as input to `point::collect`: each element's hit is the centre
/// of its body in the 3D scene (the view only knows projections), zero when it has none.
pub fn point_input(app: &EditorApp) -> crate::ai::point::PointInput {
    use crate::ai::point::{PointInput, PoseResolved, ViewKind, ViewRef};
    let hits = app
        .ed
        .selection
        .iter()
        .map(|&s| {
            let c = app
                .ed
                .element_range(s)
                .and_then(|r| app.scene.bounds_of(r))
                .map_or([0.0; 3], |b| {
                    let m = (b.min + b.max) * 0.5;
                    [m.x as f64, m.y as f64, m.z as f64]
                });
            (s, c)
        })
        .collect();
    let (kind, pose) = if app.view3d {
        let p = app.view.camera.pose();
        let t = p.target;
        let pose = PoseResolved {
            target: [t.x as f64, t.y as f64, t.z as f64],
            yaw: p.yaw as f64,
            pitch: p.pitch as f64,
            distance: p.distance as f64,
        };
        (ViewKind::View3d, Some(pose))
    } else {
        (ViewKind::Plan, None)
    };
    PointInput {
        view: ViewRef {
            kind,
            name: kind.as_str().to_string(),
            level: Some(app.ed.level),
        },
        hits,
        region: None,
        pose,
        crop: None,
    }
}

/// Identity of the open project; `None` for a model only in memory.
pub fn project_of(m: &Model) -> Option<String> {
    m.sql().path().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cloud() -> Profile {
        let mut p = blank_profile("p1".into(), "x".into());
        p.base_url = "https://models.example.test/v1".into();
        p.model = "m".into();
        p
    }

    fn local() -> Profile {
        let mut p = cloud();
        p.base_url = "http://127.0.0.1:11434/v1".into();
        p
    }

    fn model() -> Model {
        Model::open(":memory:").unwrap()
    }

    #[test]
    fn no_address_or_model_counts_as_no_profile() {
        let m = model();
        assert_eq!(gate(&m, None, None), Gate::NoProfile);
        assert_eq!(
            gate(&m, Some(&blank_profile("a".into(), "b".into())), None),
            Gate::NoProfile
        );
    }

    #[test]
    fn the_host_page_needs_no_consent_in_the_browser_and_is_no_route_elsewhere() {
        let m = model();
        m.set_ai_policy("egress.local_only", "1").unwrap();
        let p = page_profile();
        assert_eq!(gate_on(&m, Some(&p), None, true), Gate::Ready { host: None });
        assert!(!matches!(gate_on(&m, Some(&p), None, false), Gate::Ready { .. }));
        assert!(!uses_page(&cloud(), true));
        assert_eq!(default_profile().is_some(), ON_WEB);
        if !ON_WEB {
            assert!(page_provider().is_err());
        }
    }

    #[test]
    fn local_needs_no_consent_even_in_local_only() {
        let m = model();
        m.set_ai_policy("egress.local_only", "1").unwrap();
        assert_eq!(gate(&m, Some(&local()), None), Gate::Ready { host: None });
    }

    #[test]
    fn cloud_waits_for_a_consent_per_project() {
        let m = model();
        let Gate::NeedConsent { host } = gate(&m, Some(&cloud()), None) else {
            panic!("expected a consent card");
        };
        assert_eq!(
            gate(&m, Some(&cloud()), Some(&host)),
            Gate::Ready {
                host: Some(host.clone())
            }
        );
        assert!(
            matches!(gate(&m, Some(&cloud()), None), Gate::NeedConsent { .. }),
            "once is the caller's memory"
        );
        m.set_ai_policy(&logic::consent_policy(&host), "1").unwrap();
        assert_eq!(
            gate(&m, Some(&cloud()), None),
            Gate::Ready { host: Some(host) }
        );
        assert!(
            matches!(
                gate(&model(), Some(&cloud()), None),
                Gate::NeedConsent { .. }
            ),
            "another project asks again"
        );
    }

    #[test]
    fn local_only_blocks_cloud_whatever_was_consented() {
        let m = model();
        let host = consent_key(&cloud().base_url).unwrap();
        m.set_ai_policy(&logic::consent_policy(&host), "1").unwrap();
        m.set_ai_policy("egress.local_only", "1").unwrap();
        assert_eq!(gate(&m, Some(&cloud()), Some(&host)), Gate::LocalBlocked);
    }

    #[test]
    fn add_local_preset_fills_address_without_key() {
        let p = preset_profile(Preset::Local, "a".into(), "n".into());
        assert_eq!(p.base_url, LOCAL_URL);
        assert_eq!(p.auth, AuthKind::None);
        assert_eq!(p.protocol, Protocol::ChatCompletions);
        assert!(is_local(&p));
        assert_eq!(missing(&p, false), vec!["ai.settings.need_model"]);
    }

    #[test]
    fn add_cloud_preset_needs_key() {
        let mut p = preset_profile(Preset::Hosted, "a".into(), "n".into());
        assert_eq!(p.auth, AuthKind::Bearer);
        assert_eq!(
            missing(&p, false),
            vec!["ai.settings.need_address", "ai.settings.need_model", "ai.settings.need_key"]
        );
        p.base_url = "models.example.test/v1".into();
        p.model = "m".into();
        assert_eq!(missing(&p, true), vec!["ai.settings.bad_address"]);
        p.base_url = "https://models.example.test/v1".into();
        assert_eq!(missing(&p, false), vec!["ai.settings.need_key"]);
        assert!(missing(&p, true).is_empty());
    }

    #[test]
    fn gateway_preset_fills_the_address_and_asks_only_for_model_and_key() {
        let p = preset_profile(Preset::Gateway, "a".into(), "n".into());
        assert_eq!(p.base_url, ddc_ai::gateway::GATEWAY_URL);
        assert_eq!((p.auth.clone(), p.protocol), (AuthKind::Bearer, Protocol::ChatCompletions));
        assert!(!is_local(&p));
        assert_eq!(missing(&p, false), vec!["ai.settings.need_model", "ai.settings.need_key"]);
        assert!(Preset::ALL.contains(&Preset::Gateway));
    }

    #[test]
    fn custom_header_auth_needs_a_header_name() {
        let mut p = cloud();
        p.auth = AuthKind::Header {
            name: " ".into(),
        };
        assert_eq!(missing(&p, true), vec!["ai.settings.need_header_name"]);
    }

    #[test]
    fn preset_keys_in_both_languages() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            let mut keys: Vec<String> = vec![];
            for p in Preset::ALL {
                keys.push(p.key().to_string());
                keys.push(format!("{}_sub", p.key()));
            }
            for k in [
                "ai.settings.base_url_hint",
                "ai.settings.need_address",
                "ai.settings.bad_address",
                "ai.settings.need_model",
                "ai.settings.need_key",
                "ai.settings.need_header_name",
                "ai.settings.advanced",
                "ai.settings.protocol_tip",
                "ai.settings.model_hint",
            ] {
                keys.push(k.to_string());
            }
            for k in &keys {
                assert_ne!(cat.t(k), *k, "{k} in {lang:?}");
            }
        }
    }

    #[test]
    fn snapshots_are_off_until_the_person_turns_them_on() {
        let m = model();
        assert!(!snapshots_on(&m));
        m.set_ai_policy("egress.snapshots", "1").unwrap();
        assert!(snapshots_on(&m));
    }

    #[test]
    fn unanswered_snapshots_follow_the_provider_and_the_card_starts_ticked() {
        let m = model();
        let mut local = cloud();
        local.base_url = "http://127.0.0.1:11434/v1".into();
        assert!(snapshots_for(&m, Some(&local)), "local provider: images stay here");
        assert!(!snapshots_for(&m, Some(&cloud())), "remote provider: nothing without an answer");
        assert!(!snapshots_for(&m, None));
        assert!(snapshots_card_default(&m));
        m.set_ai_policy("egress.snapshots", "0").unwrap();
        assert!(!snapshots_for(&m, Some(&local)), "an explicit no beats the default");
        assert!(!snapshots_card_default(&m), "the saved no stays unticked");
        m.set_ai_policy("egress.snapshots", "1").unwrap();
        assert!(snapshots_for(&m, Some(&cloud())));
    }

    #[test]
    fn limits_come_from_the_project() {
        let m = model();
        m.set_ai_policy("limits.max_turn_tokens", "1234").unwrap();
        m.set_ai_policy("limits.max_cost", "0.5").unwrap();
        let l = limits_of(&m);
        assert_eq!(l.max_turn_tokens, 1234);
        assert_eq!(l.max_cost, Some(0.5));
    }

    #[test]
    fn a_key_left_in_a_stored_address_moves_to_the_store_on_load() {
        const FAKE: &str = "fake-SECRETSECRET-ai15";
        let d = std::env::temp_dir().join(format!("ddc-ai15-load-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut p = cloud();
        p.auth = AuthKind::None;
        p.base_url = format!("https://models.example.test/v1?key={FAKE}");
        // written the way an older version did, before save refused such an address
        let text = serde_json::json!({"version": 1, "profiles": [p]}).to_string();
        std::fs::write(ddc_ai::profile::profiles_path(&d), text).unwrap();
        let store = ddc_ai::MemoryStore::new();
        let list = load_profiles_in(&d, &store);
        assert_eq!(list[0].base_url, "https://models.example.test/v1");
        assert_eq!(list[0].auth, AuthKind::Query { name: "key".into() });
        assert!(store.get("p1").unwrap().is_some(), "the key is in the store");
        let file = std::fs::read_to_string(ddc_ai::profile::profiles_path(&d)).unwrap();
        assert!(!file.contains(FAKE), "{file}");
        assert_eq!(load_profiles_in(&d, &store), list);
    }

    /// A store that refuses writes, or (`get_fails`) cannot even be read.
    struct BrokenStore {
        get_fails: bool,
    }

    impl SecretStore for BrokenStore {
        fn get(&self, _: &str) -> Result<Option<ddc_ai::Secret>, ddc_ai::SecretError> {
            if self.get_fails {
                return Err(ddc_ai::SecretError::Store("locked".into()));
            }
            Ok(None)
        }
        fn set(&self, _: &str, _: ddc_ai::Secret) -> Result<(), ddc_ai::SecretError> {
            Err(ddc_ai::SecretError::Store("refused".into()))
        }
        fn remove(&self, _: &str) -> Result<(), ddc_ai::SecretError> {
            Ok(())
        }
    }

    /// Every file in `d` (profiles, temp files, backups) is free of `fake`.
    fn no_file_holds(d: &std::path::Path, fake: &str) {
        for e in std::fs::read_dir(d).unwrap() {
            let path = e.unwrap().path();
            let bytes = std::fs::read(&path).unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains(fake), "{} holds the key", path.display());
        }
    }

    #[test]
    fn load_profiles_keeps_address_when_store_fails() {
        const FAKE: &str = "fake-SECRETSECRET-r107b";
        for get_fails in [false, true] {
            let d = std::env::temp_dir().join(format!(
                "ddc-r107b-{}-{get_fails}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            let mut p = cloud();
            p.auth = AuthKind::Bearer;
            p.base_url = format!("https://models.example.test/v1?key={FAKE}");
            let text = serde_json::json!({"version": 1, "profiles": [p]}).to_string();
            let path = ddc_ai::profile::profiles_path(&d);
            std::fs::write(&path, &text).unwrap();
            let list = load_profiles_in(&d, &BrokenStore { get_fails });
            assert_eq!(list[0].base_url, p.base_url, "the key stays in memory");
            // the disk copy is the clean address only, with no temp or backup left
            let on_disk = ddc_ai::profile::load_profiles(&d).unwrap();
            assert_eq!(on_disk[0].base_url, "https://models.example.test/v1");
            no_file_holds(&d, FAKE);
            assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "only the list is there");
            // a save still cannot write it in plain text
            let e = save_profiles_in(Some(d.clone()), &list).unwrap_err();
            assert!(!e.contains(FAKE), "{e}");
            no_file_holds(&d, FAKE);
            // after a restart the key is gone and is asked for again
            let again = load_profiles_in(&d, &BrokenStore { get_fails });
            assert_eq!(again[0].base_url, "https://models.example.test/v1");
        }
    }

    #[test]
    fn load_profiles_cleans_stored_keys_when_one_is_held() {
        const HELD: &str = "fake-SECRETSECRET-r111a-held";
        const KEPT: &str = "fake-SECRETSECRET-r111a-kept";
        let d = std::env::temp_dir().join(format!("ddc-r111a-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut a = cloud();
        a.base_url = format!("https://models.example.test/v1?key={HELD}");
        let mut b = cloud();
        b.id = "p2".into();
        b.base_url = format!("https://other.example.test/v1?key={KEPT}");
        let text = serde_json::json!({"version": 1, "profiles": [a, b]}).to_string();
        std::fs::write(ddc_ai::profile::profiles_path(&d), text).unwrap();
        // a stray temp file of a failed older write is replaced, not kept beside
        let tmp = ddc_ai::profile::profiles_path(&d).with_extension("json.tmp");
        std::fs::write(&tmp, HELD).unwrap();

        /// Takes every key except the one of `p1`.
        struct RefusesP1(ddc_ai::MemoryStore);
        impl SecretStore for RefusesP1 {
            fn get(&self, id: &str) -> Result<Option<ddc_ai::Secret>, ddc_ai::SecretError> {
                self.0.get(id)
            }
            fn set(&self, id: &str, k: ddc_ai::Secret) -> Result<(), ddc_ai::SecretError> {
                if id == "p1" {
                    return Err(ddc_ai::SecretError::Store("refused".into()));
                }
                self.0.set(id, k)
            }
            fn remove(&self, id: &str) -> Result<(), ddc_ai::SecretError> {
                self.0.remove(id)
            }
        }
        let store = RefusesP1(ddc_ai::MemoryStore::new());
        let list = load_profiles_in(&d, &store);
        assert!(list[0].base_url.contains(HELD), "the held key stays in memory");
        assert_eq!(list[1].base_url, "https://other.example.test/v1");
        assert!(store.get("p2").unwrap().is_some(), "the other key is in the store");
        assert!(!tmp.exists(), "the temp file was renamed over the list");
        no_file_holds(&d, HELD);
        no_file_holds(&d, KEPT);
    }

    #[test]
    fn load_profiles_never_leaves_a_credential_header_on_disk() {
        const MOVED: &str = "fake-SECRETSECRET-key4a-moved";
        const HELD: &str = "fake-SECRETSECRET-key4a-held";
        const ADDR: &str = "fake-SECRETSECRET-key4a-addr";
        const OLD: &str = "fake-SECRETSECRET-key4a-stored";
        let d = std::env::temp_dir().join(format!("ddc-key4a-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // no stored key: the header becomes the profile key
        let mut a = cloud();
        a.extra_headers = vec![
            ("x-version".into(), "1".into()),
            ("X-Api-Key".into(), MOVED.into()),
        ];
        // a stored key and a key in the address: the header stays in memory only
        let mut b = cloud();
        b.id = "p2".into();
        b.base_url = format!("https://other.example.test/v1?key={ADDR}");
        b.extra_headers = vec![("Authorization".into(), HELD.into())];
        let text = serde_json::json!({"version": 1, "profiles": [a, b]}).to_string();
        std::fs::write(ddc_ai::profile::profiles_path(&d), text).unwrap();
        let store = ddc_ai::MemoryStore::new();
        store.set("p2", ddc_ai::Secret::new(OLD)).unwrap();

        let list = load_profiles_in(&d, &store);
        assert_eq!(list[0].auth, AuthKind::Header { name: "X-Api-Key".into() });
        let version = vec![("x-version".to_string(), "1".to_string())];
        assert_eq!(list[0].extra_headers, version);
        assert!(store.get("p1").unwrap() == Some(ddc_ai::Secret::new(MOVED)));
        assert!(list[1].extra_headers.iter().any(|(_, v)| v == HELD), "kept in memory");
        assert!(store.get("p2").unwrap() == Some(ddc_ai::Secret::new(OLD)), "store wins");
        for fake in [MOVED, HELD, ADDR, OLD] {
            no_file_holds(&d, fake);
        }
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "only the list is there");
        let on_disk = ddc_ai::profile::load_profiles(&d).unwrap();
        assert_eq!(on_disk[0].auth, list[0].auth);
        assert_eq!(on_disk[0].extra_headers, list[0].extra_headers);
        assert!(on_disk[1].extra_headers.is_empty());
        // a save of the held header is still refused, never written in plain text
        let e = save_profiles_in(Some(d.clone()), &list).unwrap_err();
        assert!(!e.contains(HELD), "{e}");
        for fake in [MOVED, HELD, ADDR, OLD] {
            no_file_holds(&d, fake);
        }
    }

    /// Every file under `d`, `_archive` included, is free of `fake`.
    fn no_file_under_holds(d: &std::path::Path, fake: &str) {
        for e in std::fs::read_dir(d).unwrap() {
            let path = e.unwrap().path();
            if path.is_dir() {
                no_file_under_holds(&path, fake);
                continue;
            }
            let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).to_string();
            assert!(!text.contains(fake), "{} holds a fixture key", path.display());
        }
    }

    /// A key under an innocent header name (found by its value) leaves the file on
    /// load like a named one; an ordinary header stays.
    #[test]
    fn a_key_shaped_header_value_leaves_the_file_on_load() {
        const BEARER: &str = "Qz7fixtureR114bXk2Lp9W";
        const SK: &str = "sk-fixture-r114b-not-a-real-key";
        let d = std::env::temp_dir().join(format!("ddc-r114b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut a = cloud();
        a.extra_headers = vec![
            ("x-version".into(), "2023-06-01".into()),
            ("x-trace".into(), format!("Bearer {BEARER}")),
        ];
        let mut b = cloud();
        b.id = "p2".into();
        b.extra_headers = vec![("x-custom".into(), SK.into())];
        let text = serde_json::json!({"version": 1, "profiles": [a, b]}).to_string();
        std::fs::write(ddc_ai::profile::profiles_path(&d), text).unwrap();
        let store = ddc_ai::MemoryStore::new();
        store.set("p2", ddc_ai::Secret::new("fake-stored-r114b")).unwrap();

        let list = load_profiles_in(&d, &store);
        assert_eq!(list[0].auth, AuthKind::Header { name: "x-trace".into() });
        let version = vec![("x-version".to_string(), "2023-06-01".to_string())];
        assert_eq!(list[0].extra_headers, version, "the plain header stays");
        assert!(list[1].extra_headers.iter().any(|(_, v)| v == SK), "held in memory");
        for fake in [BEARER, SK, "fake-stored-r114b"] {
            no_file_under_holds(&d, fake);
        }
        let on_disk = ddc_ai::profile::load_profiles(&d).unwrap();
        assert_eq!(on_disk[0].extra_headers, version);
        assert!(on_disk[1].extra_headers.is_empty());
        let e = save_profiles_in(Some(d.clone()), &list).unwrap_err();
        assert!(!e.contains(SK), "a save of the held header is refused, the key not named");
        no_file_under_holds(&d, SK);
    }

    /// A list file that is not a list moves to `_archive` with its keys masked and the
    /// list starts empty; the notice names the copy in both languages and no key.
    #[test]
    fn an_unreadable_list_is_archived_masked_with_a_notice() {
        const HDR: &str = "fake-SECRETSECRET-r114a-app-hdr";
        const ADDR: &str = "fake-SECRETSECRET-r114a-app-addr";
        const SK: &str = "sk-fixture-r114a-not-a-real-key";
        let d = std::env::temp_dir().join(format!("ddc-r114a-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let text = format!(
            "{{\"version\":1,\"profiles\":[{{\"id\":\"p1\",\"name\":\"Office server\",\
             \"base_url\":\"https://models.example.test/v1?api_key={ADDR}\",\
             \"extra_headers\":[[\"Authorization\",\"{HDR}\"],[\"x-custom\",\"{SK}\"]"
        );
        std::fs::write(ddc_ai::profile::profiles_path(&d), text).unwrap();
        let store = ddc_ai::MemoryStore::new();
        let (list, u) = load_profiles_noting(&d, &store);
        assert!(list.is_empty());
        let got = match u {
            Some(Unreadable::Archived(got)) => got,
            other => panic!("not archived: {other:?}"),
        };
        assert!(got.scrubbed);
        assert!(got.path.starts_with(d.join("_archive")));
        assert!(!ddc_ai::profile::profiles_path(&d).exists(), "moved, not copied");
        for fake in [HDR, ADDR, SK] {
            no_file_under_holds(&d, fake);
        }
        let copy = std::fs::read_to_string(&got.path).unwrap();
        assert!(copy.contains("Office server") && copy.contains("models.example.test/v1"));
        let shown = got.path.display().to_string();
        let u = Unreadable::Archived(got);
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            let note = unreadable_note(&cat, &u);
            assert!(note.contains(&shown), "{lang:?}: the notice names the copy");
            assert!(!note.contains("ai.settings"), "{lang:?}: {note}");
            for fake in [HDR, ADDR, SK] {
                assert!(!note.contains(fake), "{lang:?}");
            }
            for k in [
                "ai.settings.profiles_archived",
                "ai.settings.profiles_archived_raw",
                "ai.settings.profiles_unreadable",
            ] {
                assert_ne!(cat.t(k), k, "{k} in {lang:?}");
            }
            let stayed = unreadable_note(&cat, &Unreadable::Stayed("denied".into()));
            assert!(stayed.contains("denied"), "{lang:?}: {stayed}");
        }
        // the next start finds no list and no notice
        let (list, u) = load_profiles_noting(&d, &store);
        assert!(list.is_empty() && u.is_none());
        assert!(store.get("p1").unwrap().is_none(), "nothing from the damaged file was kept");
    }

    #[test]
    fn a_stored_key_wins_and_keeps_the_key_mode() {
        const FAKE: &str = "fake-SECRETSECRET-r107c";
        let d = std::env::temp_dir().join(format!("ddc-r107c-load-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut p = cloud();
        p.auth = AuthKind::Bearer;
        p.base_url = format!("https://models.example.test/v1?key={FAKE}");
        let text = serde_json::json!({"version": 1, "profiles": [p]}).to_string();
        std::fs::write(ddc_ai::profile::profiles_path(&d), text).unwrap();
        let store = ddc_ai::MemoryStore::new();
        store.set(&p.id, ddc_ai::Secret::new("fake-stored-r107c")).unwrap();
        let list = load_profiles_in(&d, &store);
        assert_eq!(list[0].base_url, "https://models.example.test/v1");
        assert_eq!(list[0].auth, AuthKind::Bearer, "the stored key goes as before");
        let file = std::fs::read_to_string(ddc_ai::profile::profiles_path(&d)).unwrap();
        assert!(!file.contains(FAKE), "{file}");
    }

    #[test]
    fn save_without_a_data_folder_says_why() {
        let e = save_profiles_in(None, &[cloud()]).unwrap_err();
        assert!(!e.trim().is_empty());
        assert_ne!(e, "ai.settings.no_data_dir");
    }
}
