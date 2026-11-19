//! The "what was sent" privacy log: one entry per request that left the machine, with when,
//! to which host, for which model, how large, and which parts were project data. It lives in
//! memory only, is bounded in count and bytes, and never holds a key: the provider scrubs the
//! body before it gets here, and `SentEntry::new` scrubs again.

use super::kit::{self, Button, Dialog, DialogSize};
use super::{consent, logic, Cx, PanelUi};
use ddc_ai::profile::is_secret_header;
use ddc_ai::secret::scrub_known;
use ddc_ui::theme::Role;
use ddc_ui::{Catalog, Theme};
use egui::{self, RichText, TextEdit, Ui};
use serde_json::Value as J;
use std::collections::VecDeque;

/// Oldest requests leave first.
pub const SENT_KEPT: usize = 64;
/// All kept entries together, raw and pretty text counted.
pub const SENT_BYTES: usize = 8 << 20;
/// What one body shows; Copy still takes the whole kept text.
const SENT_CLIP: usize = 200_000;
const HIDDEN: &str = "***";

/// One request as it left. No field for a key or header: nothing secret has a place here.
#[derive(Clone, Debug, PartialEq)]
pub struct SentEntry {
    /// Unix seconds at dispatch.
    pub at: i64,
    pub host: String,
    pub model: String,
    pub text: String,
}

impl SentEntry {
    /// `endpoint` is the profile base URL; only its host and port stay.
    pub fn new(at: i64, endpoint: &str, model: &str, text: &str) -> Self {
        SentEntry {
            at,
            host: host_of(endpoint),
            model: scrub_known(model),
            text: scrub_body(text),
        }
    }

    pub fn now(endpoint: &str, model: &str, text: &str) -> Self {
        let at = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        Self::new(at, endpoint, model, text)
    }
}

/// `https://user:pw@api.example.com:8443/v1?k=x` -> `api.example.com:8443`: credentials in
/// the authority and anything in the path or query never reach the log.
pub fn host_of(endpoint: &str) -> String {
    let s = endpoint.trim();
    let s = s.split_once("://").map_or(s, |(_, rest)| rest);
    let end = s.find(['/', '?', '#']).unwrap_or(s.len());
    let auth = &s[..end];
    let host = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    scrub_known(host)
}

/// Known keys out anywhere; a string under a credential-like name at the top level or in a
/// `headers` object is hidden even when the key store never saw it.
pub fn scrub_body(text: &str) -> String {
    let out = scrub_known(text);
    let Ok(mut doc) = serde_json::from_str::<J>(&out) else {
        return out;
    };
    if hide_secret_fields(&mut doc, true) {
        serde_json::to_string(&doc).unwrap_or(out)
    } else {
        out
    }
}

fn hide_secret_fields(v: &mut J, here: bool) -> bool {
    let mut hit = false;
    match v {
        J::Object(fields) => {
            for (k, x) in fields.iter_mut() {
                if here && is_secret_header(k) {
                    if let J::String(s) = x {
                        if s != HIDDEN {
                            *s = HIDDEN.to_string();
                            hit = true;
                        }
                    }
                }
                hit |= hide_secret_fields(x, k.eq_ignore_ascii_case("headers"));
            }
        }
        J::Array(items) => {
            for x in items {
                hit |= hide_secret_fields(x, false);
            }
        }
        _ => {}
    }
    hit
}

/// The project data a request carried: the person's messages, tool results (element names,
/// parameters) and attached images (snapshots, already reduced to a size note).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub messages: usize,
    pub tool_results: usize,
    pub images: usize,
}

fn is_image(part: &J) -> bool {
    matches!(
        part.get("type").and_then(J::as_str),
        Some("image" | "image_url" | "input_image")
    )
}

fn count_images(v: &J) -> usize {
    match v {
        J::Array(items) => items.iter().map(count_images).sum(),
        J::Object(_) if is_image(v) => 1,
        J::Object(fields) => fields.values().map(count_images).sum(),
        _ => 0,
    }
}

/// `None` when the text is not a JSON request with a message list.
pub fn sent_summary(text: &str) -> Option<Summary> {
    let doc: J = serde_json::from_str(text).ok()?;
    let msgs = doc.get("messages")?.as_array()?;
    let mut s = Summary::default();
    for m in msgs {
        let content = m.get("content").unwrap_or(&J::Null);
        match m.get("role").and_then(J::as_str).unwrap_or("") {
            "tool" => {
                s.tool_results += 1;
                s.images += count_images(content);
            }
            "user" => match content {
                J::String(_) => s.messages += 1,
                J::Array(parts) => {
                    let mut text = false;
                    for p in parts {
                        match p.get("type").and_then(J::as_str) {
                            Some("tool_result") => {
                                s.tool_results += 1;
                                s.images += count_images(p.get("content").unwrap_or(&J::Null));
                            }
                            _ if is_image(p) => s.images += 1,
                            Some("text" | "input_text") => text = true,
                            _ => {}
                        }
                    }
                    s.messages += usize::from(text);
                }
                _ => {}
            },
            _ => {}
        }
    }
    Some(s)
}

/// Pretty JSON, or the text as it is.
pub fn pretty(text: &str) -> String {
    match serde_json::from_str::<J>(text) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| text.to_string()),
        Err(_) => text.to_string(),
    }
}

/// The body split for reading: the project part first, the fixed instructions (system prompt
/// and tool schemas) apart, so the person sees their own data without scrolling past them.
#[derive(Clone, Debug, PartialEq)]
pub struct Sections {
    pub project: String,
    pub instructions: Option<String>,
}

fn is_instruction_role(m: &J) -> bool {
    matches!(
        m.get("role").and_then(J::as_str),
        Some("system" | "developer")
    )
}

pub fn sections(text: &str) -> Sections {
    let Ok(J::Object(mut doc)) = serde_json::from_str::<J>(text) else {
        return Sections {
            project: pretty(text),
            instructions: None,
        };
    };
    let mut ins = serde_json::Map::new();
    for k in ["system", "tools", "tool_choice"] {
        if let Some(v) = doc.remove(k) {
            ins.insert(k.to_string(), v);
        }
    }
    if let Some(J::Array(msgs)) = doc.get_mut("messages") {
        let (sys, rest): (Vec<J>, Vec<J>) = msgs.drain(..).partition(is_instruction_role);
        *msgs = rest;
        if !sys.is_empty() {
            ins.insert("messages".to_string(), J::Array(sys));
        }
    }
    let show = |v: &J| serde_json::to_string_pretty(v).unwrap_or_default();
    Sections {
        project: show(&J::Object(doc)),
        instructions: (!ins.is_empty()).then(|| show(&J::Object(ins))),
    }
}

/// `14:03 · api.example.com · model-x · 18.4 KB`; an empty model is left out.
pub fn header_text(cat: &Catalog, e: &SentEntry) -> String {
    let local = logic::fmt_local(e.at);
    let time = local.split_whitespace().nth(1).unwrap_or(local.as_str()).to_string();
    let mut parts = vec![time, e.host.clone()];
    if !e.model.is_empty() {
        parts.push(e.model.clone());
    }
    parts.push(crate::ui_kit::fmt_size(cat, e.text.len() as u64));
    parts.join(" \u{b7} ")
}

pub fn summary_text(cat: &Catalog, s: Summary) -> String {
    cat.tf(
        "ai_sent.summary",
        &[
            ("messages", &s.messages.to_string()),
            ("tools", &s.tool_results.to_string()),
            ("images", &s.images.to_string()),
        ],
    )
}

/// Indices of entries whose host, model or text holds `q`, any case; all on an empty `q`.
pub fn filter(entries: &[&SentEntry], q: &str) -> Vec<usize> {
    let q = q.trim().to_lowercase();
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            q.is_empty()
                || [&e.host, &e.model, &e.text]
                    .iter()
                    .any(|f| f.to_lowercase().contains(&q))
        })
        .map(|(i, _)| i)
        .collect()
}

struct Kept {
    e: SentEntry,
    serial: u64,
    summary: Option<Summary>,
    view: Sections,
}

impl Kept {
    fn bytes(&self) -> usize {
        self.e.text.len()
            + self.view.project.len()
            + self.view.instructions.as_ref().map_or(0, String::len)
    }
}

/// The kept requests. Parsing happens once, on arrival, not every frame.
#[derive(Default)]
pub struct SentLog {
    items: VecDeque<Kept>,
    bytes: usize,
    serial: u64,
}

impl SentLog {
    pub fn push(&mut self, e: SentEntry) {
        self.push_capped(e, SENT_KEPT, SENT_BYTES);
    }

    fn push_capped(&mut self, mut e: SentEntry, max_n: usize, max_bytes: usize) {
        if e.text.len() > max_bytes {
            e.text = consent::clip(&e.text, max_bytes).to_string();
        }
        let mut view = sections(&e.text);
        if e.text.len() + view.project.len() > max_bytes {
            view.project = e.text.clone();
            view.instructions = None;
        }
        let k = Kept {
            summary: sent_summary(&e.text),
            view,
            serial: self.serial,
            e,
        };
        self.serial += 1;
        self.bytes += k.bytes();
        self.items.push_back(k);
        while self.items.len() > max_n.max(1)
            || (self.bytes > max_bytes && self.items.len() > 1)
        {
            if let Some(old) = self.items.pop_front() {
                self.bytes -= old.bytes();
            }
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.bytes = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn entries(&self) -> Vec<&SentEntry> {
        self.items.iter().map(|k| &k.e).collect()
    }
}

/// The filter text and its last result, kept until the text or the log changes.
#[derive(Default)]
pub struct SentView {
    pub q: String,
    cache: Option<(String, u64, usize, Vec<usize>)>,
}

impl SentView {
    fn hits(&mut self, log: &SentLog) -> Vec<usize> {
        let key = (log.serial, log.items.len());
        if let Some((q, s, n, hits)) = &self.cache {
            if *q == self.q && (*s, *n) == key {
                return hits.clone();
            }
        }
        let hits = filter(&log.entries(), &self.q);
        self.cache = Some((self.q.clone(), key.0, key.1, hits.clone()));
        hits
    }
}

pub fn window(ctx: &egui::Context, st: &mut PanelUi, cx: &Cx<'_>) {
    if !st.sent_open {
        return;
    }
    let (th, cat) = (cx.th, cx.cat);
    let title = cat.t("cmd.ai.what_sent");
    let PanelUi {
        sent,
        sent_view,
        sent_open,
        ..
    } = st;
    Dialog::new("ai.sent", &title, DialogSize::Wide)
        .open(sent_open)
        .scroll_body()
        .show(ctx, |ui| {
            if sent.is_empty() {
                kit::caption(ui, th, &cat.t("ai.sent.empty"));
                return;
            }
            kit::caption(ui, th, &cat.t("ai_sent.local_note"));
            ui.add(
                TextEdit::singleline(&mut sent_view.q)
                    .hint_text(cat.t("ai_sent.filter"))
                    .desired_width(f32::INFINITY),
            );
            let hits = sent_view.hits(sent);
            if hits.is_empty() {
                kit::caption(ui, th, &cat.t("ai_sent.no_match"));
                return;
            }
            let last = sent.len() - 1;
            for &i in hits.iter().rev() {
                let k = &sent.items[i];
                egui::CollapsingHeader::new(
                    RichText::new(header_text(cat, &k.e)).font(th.font(Role::Label)),
                )
                .id_salt(("ddc.ai.sent", k.serial))
                .default_open(i == last)
                .show(ui, |ui| body(ui, th, cat, k));
            }
        });
}

fn body(ui: &mut Ui, th: &Theme, cat: &Catalog, k: &Kept) {
    ui.horizontal(|ui| {
        if let Some(s) = k.summary {
            kit::caption(ui, th, &summary_text(cat, s));
        }
        if Button::quiet(&cat.t("ai_sent.copy")).small().show(ui, th).clicked() {
            ui.ctx().copy_text(k.e.text.clone());
        }
    });
    mono(ui, th, cat, &k.view.project);
    if let Some(ins) = &k.view.instructions {
        egui::CollapsingHeader::new(cat.t("ai_sent.instructions"))
            .id_salt(("ddc.ai.sent.ins", k.serial))
            .default_open(false)
            .show(ui, |ui| mono(ui, th, cat, ins));
    }
}

fn mono(ui: &mut Ui, th: &Theme, cat: &Catalog, text: &str) {
    let shown = consent::clip(text, SENT_CLIP);
    ui.add(
        egui::Label::new(
            RichText::new(shown)
                .font(th.font(Role::Numeric))
                .color(th.c("text.primary")),
        )
        .selectable(true)
        .wrap(),
    );
    if shown.len() < text.len() {
        kit::caption(ui, th, &cat.t("ai.sent.clipped"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::Lang;

    const REQ: &str = r#"{"model":"m1","system":"You are an assistant.","tools":[{"name":"q","input_schema":{"type":"object"}}],"messages":[{"role":"user","content":[{"type":"text","text":"Walls on level 2"},{"type":"image","source":{"type":"base64","data":"[image 4x4, 64 bytes]"}}]},{"role":"assistant","content":[{"type":"tool_use","id":"c1","name":"q","input":{"key":"Mark"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":[{"type":"text","text":"W1 Basic wall"}]}]}]}"#;

    fn entry(text: &str) -> SentEntry {
        SentEntry::new(0, "https://api.example.com/v1", "m1", text)
    }

    #[test]
    fn header_uses_fmt_size_host_and_model() {
        let mut e = entry(&"x".repeat(999));
        let cat = Catalog::new(ddc_ui::Lang::En);
        let h = header_text(&cat, &e);
        assert!(h.ends_with(" \u{b7} api.example.com \u{b7} m1 \u{b7} 999 B"), "{h}");
        assert_eq!(h.split(" \u{b7} ").next().unwrap().len(), "00:00".len());
        e.text = "x".repeat(18_842);
        assert!(header_text(&cat, &e).ends_with(" \u{b7} 18.4 KB"));
        e.model.clear();
        assert!(header_text(&cat, &e).ends_with(" \u{b7} api.example.com \u{b7} 18.4 KB"));
    }

    #[test]
    fn summary_counts_messages_tool_results_and_images() {
        let s = sent_summary(REQ).unwrap();
        assert_eq!(
            s,
            Summary {
                messages: 1,
                tool_results: 1,
                images: 1
            }
        );
        let chat = r#"{"messages":[{"role":"system","content":"sys"},{"role":"user","content":"hi"},{"role":"tool","tool_call_id":"a","content":"rows"},{"role":"user","content":[{"type":"text","text":"Image returned"},{"type":"image_url","image_url":{"url":"data:image/png;base64,[image 2x2, 9 bytes]"}}]}]}"#;
        assert_eq!(
            sent_summary(chat).unwrap(),
            Summary {
                messages: 2,
                tool_results: 1,
                images: 1
            }
        );
        assert_eq!(sent_summary("not json"), None);
    }

    #[test]
    fn sections_put_project_data_first_and_instructions_apart() {
        let s = sections(REQ);
        assert!(s.project.contains("Walls on level 2"));
        assert!(!s.project.contains("You are an assistant."));
        assert!(!s.project.contains("input_schema"));
        let ins = s.instructions.unwrap();
        assert!(ins.contains("You are an assistant.") && ins.contains("input_schema"));
        let chat = r#"{"messages":[{"role":"system","content":"rules"},{"role":"user","content":"hi"}]}"#;
        let s = sections(chat);
        assert!(!s.project.contains("rules") && s.instructions.unwrap().contains("rules"));
        let s = sections(r#"{"messages":[]}"#);
        assert_eq!(s.instructions, None);
    }

    #[test]
    fn pretty_leaves_non_json_as_is() {
        assert_eq!(pretty("plain text {"), "plain text {");
        assert_eq!(pretty(r#"{"a":1}"#), "{\n  \"a\": 1\n}");
        assert_eq!(sections("plain").project, "plain");
    }

    #[test]
    fn filter_is_case_insensitive_over_host_model_and_text() {
        let a = entry("Walls on Level 2");
        let b = SentEntry::new(0, "http://localhost:11434", "Local-M", "doors");
        let all = [&a, &b];
        assert_eq!(filter(&all, ""), vec![0, 1]);
        assert_eq!(filter(&all, "  LEVEL "), vec![0]);
        assert_eq!(filter(&all, "localhost"), vec![1]);
        assert_eq!(filter(&all, "local-m"), vec![1]);
        assert!(filter(&all, "nothing").is_empty());
    }

    #[test]
    fn known_key_is_scrubbed_from_header_body_host_and_model() {
        let key = "sk-sent-test-0123456789abcdef";
        let _s = ddc_ai::Secret::new(key);
        let body = format!(
            r#"{{"headers":{{"X-Custom":"{key}"}},"messages":[{{"role":"user","content":"my key is {key}"}}]}}"#
        );
        let e = SentEntry::new(
            0,
            &format!("https://u:{key}@api.example.com/v1?key={key}"),
            key,
            &body,
        );
        assert_eq!(e.host, "api.example.com");
        for f in [&e.host, &e.model, &e.text] {
            assert!(!f.contains(key), "{f}");
        }
        assert!(e.text.contains("my key is ***"));
    }

    #[test]
    fn unknown_credential_fields_are_hidden_but_tool_inputs_are_not() {
        let e = entry(
            r#"{"api_key":"plain-unknown-1","max_tokens":10,"headers":{"Authorization":"Bearer zz-unknown"},"messages":[{"role":"assistant","content":[{"type":"tool_use","input":{"key":"Mark"}}]}]}"#,
        );
        assert!(!e.text.contains("plain-unknown-1"));
        assert!(!e.text.contains("zz-unknown"));
        assert!(e.text.contains("\"Mark\""), "{}", e.text);
        assert!(e.text.contains("\"max_tokens\":10"));
        assert_eq!(host_of("api.example.com:8443"), "api.example.com:8443");
        assert_eq!(host_of("https://h.example/x#frag"), "h.example");
    }

    #[test]
    fn entry_struct_has_no_key_or_secret_field() {
        let src = include_str!("sent.rs");
        let start = src.find("pub struct SentEntry {").unwrap();
        let def = &src[start..start + src[start..].find('}').unwrap()];
        let fields: Vec<&str> = def
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("pub ") && l.contains(':'))
            .collect();
        assert_eq!(fields.len(), 4, "{fields:?}");
        for f in fields {
            let l = f.to_ascii_lowercase();
            assert!(!l.contains("key") && !l.contains("secret") && !l.contains("header"), "{f}");
        }
    }

    #[test]
    fn one_entry_per_send_and_rotation_by_count_and_bytes() {
        let mut log = SentLog::default();
        assert!(log.is_empty() && log.entries().is_empty());
        for i in 0..5 {
            log.push_capped(entry(&format!("req {i}")), 3, 1 << 20);
        }
        assert_eq!(log.len(), 3);
        let kept = log.entries();
        let texts: Vec<&str> = kept.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["req 2", "req 3", "req 4"]);

        let mut log = SentLog::default();
        for _ in 0..4 {
            log.push_capped(entry(&"y".repeat(400)), 100, 1000);
        }
        assert!(log.bytes() <= 1000, "{}", log.bytes());
        assert_eq!(log.len(), 1);
        log.push_capped(entry(&"z".repeat(5000)), 100, 1000);
        assert_eq!(log.len(), 1);
        assert!(log.bytes() <= 2000 && log.entries()[0].text.len() == 1000);
        log.clear();
        assert!(log.is_empty() && log.bytes() == 0);
        log.push(entry("x"));
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn filter_cache_follows_query_and_log() {
        let mut log = SentLog::default();
        let mut v = SentView::default();
        assert!(v.hits(&log).is_empty());
        log.push(entry("alpha"));
        log.push(entry("beta"));
        assert_eq!(v.hits(&log), vec![0, 1]);
        v.q = "BETA".into();
        assert_eq!(v.hits(&log), vec![1]);
        log.push(entry("beta two"));
        assert_eq!(v.hits(&log), vec![1, 2]);
    }

    #[test]
    fn strings_exist_in_en_and_ru() {
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            for k in [
                "ai_sent.summary",
                "ai_sent.copy",
                "ai_sent.filter",
                "ai_sent.no_match",
                "ai_sent.instructions",
                "ai_sent.local_note",
                "ai.sent.empty",
                "ai.sent.clipped",
            ] {
                assert!(cat.table(lang).contains_key(k), "{lang:?} {k}");
            }
            let s = summary_text(
                &cat,
                Summary {
                    messages: 2,
                    tool_results: 3,
                    images: 4,
                },
            );
            assert!(s.contains('2') && s.contains('3') && s.contains('4'), "{s}");
            assert!(!s.contains('{'), "{s}");
        }
        let en = Catalog::new(Lang::En);
        assert_eq!(en.t("ai.sent.empty"), "Nothing was sent yet.");
    }
}
