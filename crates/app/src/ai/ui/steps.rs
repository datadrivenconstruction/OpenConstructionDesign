// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The assistant's tool steps in the dialog: one quiet card per run with an icon, the name and
//! the duration of every step, and a readable reason when one failed. The card only shows what
//! happened; nothing in it runs a tool or applies anything.

use super::kit::{self, space};
use super::logic;
use super::Cx;
use ddc_ui::i18n::Catalog;
use ddc_ui::theme::Role;
use ddc_ui::widgets;
use ddc_ui::Theme;
use egui::{self, Align, Color32, RichText, Sense, Ui, Vec2};

/// Longest failure text kept for a step, in characters.
const DETAIL_MAX: usize = ddc_ai::limits::UI_DETAIL.us();
/// Longest tool name accepted from a saved chat.
const NAME_MAX: usize = ddc_ai::limits::STEP_NAME.us();
/// Most steps of one run kept in the saved chat.
pub const SAVED_PER_RUN: usize = 16;

/// Which steps of a run go into the saved chat: all of a short run; of a long one the latest
/// failures first, then the latest other steps, in their order.
pub fn saved(run: &[&Step]) -> Vec<usize> {
    if run.len() <= SAVED_PER_RUN {
        return (0..run.len()).collect();
    }
    let failed: Vec<usize> = (0..run.len())
        .filter(|&i| run[i].state() == State::Failed)
        .collect();
    let mut keep = failed[failed.len().saturating_sub(SAVED_PER_RUN)..].to_vec();
    for i in (0..run.len()).rev() {
        if keep.len() >= SAVED_PER_RUN {
            break;
        }
        if run[i].state() != State::Failed {
            keep.push(i);
        }
    }
    keep.sort_unstable();
    keep
}

/// The tool's display name; a name without a text (unknown or from an edited file) is generic.
pub fn tool_name(cat: &Catalog, name: &str) -> String {
    let key = logic::tool_key(name);
    let t = cat.t(&key);
    if t == key {
        cat.t("ai.step.unknown")
    } else {
        t
    }
}

/// One tool call of the assistant.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// Catalog name of the tool (never text from the model).
    pub name: String,
    /// `None` while it runs (or when it was stopped).
    pub ok: Option<bool>,
    /// Wall clock in ms when it began; not saved with the chat.
    pub started_ms: u64,
    /// How long it took; `None` while it runs.
    pub ms: Option<u32>,
    /// The failure text, masked and shortened.
    pub err: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    Ok,
    Failed,
    /// The turn was stopped while the step ran.
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Read,
    Query,
    Draft,
}

/// Wall clock in ms; only differences are shown.
pub fn now_ms() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Step {
    pub fn start(name: &str, now_ms: u64) -> Step {
        Step {
            name: name.to_string(),
            ok: None,
            started_ms: now_ms,
            ms: None,
            err: None,
        }
    }

    pub fn state(&self) -> State {
        match (self.ok, self.ms) {
            (Some(true), _) => State::Ok,
            (Some(false), _) => State::Failed,
            (None, None) => State::Running,
            (None, Some(_)) => State::Cancelled,
        }
    }

    fn elapsed(&self, now_ms: u64) -> u32 {
        now_ms.saturating_sub(self.started_ms).min(u32::MAX as u64) as u32
    }

    /// The tool answered.
    pub fn finish(&mut self, ok: bool, err: Option<&str>, now_ms: u64) {
        self.ok = Some(ok);
        self.ms = Some(self.elapsed(now_ms));
        self.err = if ok { None } else { err.and_then(clean_detail) };
    }

    /// The turn ended under a running step: failed when `reason` is given, else stopped.
    pub fn stop(&mut self, reason: Option<&str>, now_ms: u64) {
        if self.state() != State::Running {
            return;
        }
        match reason {
            Some(r) => self.finish(false, Some(r), now_ms),
            None => self.ms = Some(self.elapsed(now_ms)),
        }
    }

    /// Time to show: live for a running step.
    pub fn duration(&self, now_ms: u64) -> u64 {
        match self.state() {
            State::Running => now_ms.saturating_sub(self.started_ms),
            _ => self.ms.unwrap_or(0) as u64,
        }
    }

    pub fn group(&self) -> Group {
        group_of(&self.name)
    }

    /// The saved form: a JSON object, optional fields left out.
    pub fn to_row(&self) -> String {
        let mut o = serde_json::Map::new();
        o.insert("n".into(), self.name.clone().into());
        if let Some(ok) = self.ok {
            o.insert("ok".into(), ok.into());
        }
        if let Some(ms) = self.ms {
            o.insert("ms".into(), ms.into());
        }
        if let Some(e) = &self.err {
            o.insert("e".into(), e.clone().into());
        }
        serde_json::Value::Object(o).to_string()
    }

    /// Older chats have no such rows, and a row may lack any optional field.
    pub fn from_row(text: &str) -> Option<Step> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let name = v.get("n")?.as_str()?;
        let plain = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_';
        if name.is_empty() || name.chars().count() > NAME_MAX || !name.chars().all(plain) {
            return None;
        }
        let ok = v.get("ok").and_then(|x| x.as_bool());
        let mut ms = v
            .get("ms")
            .and_then(|x| x.as_u64())
            .map(|x| x.min(u32::MAX as u64) as u32);
        if ok.is_none() && ms.is_none() {
            // it cannot still be running after a reload
            ms = Some(0);
        }
        let err = v
            .get("e")
            .and_then(|x| x.as_str())
            .and_then(clean_detail)
            .filter(|_| ok == Some(false));
        Some(Step {
            name: name.to_string(),
            ok,
            started_ms: 0,
            ms,
            err,
        })
    }
}

/// One masked line, cut with an ellipsis; `None` when nothing is left.
pub fn clean_detail(raw: &str) -> Option<String> {
    let one_line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let masked = logic::mask_secrets(&one_line);
    if masked.is_empty() {
        return None;
    }
    Some(elide_chars(&masked, DETAIL_MAX))
}

/// At most `max` characters, the last one an ellipsis when something was cut.
pub fn elide_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}\u{2026}")
}

pub fn group_of(name: &str) -> Group {
    const DRAFT: [&str; 12] = [
        "create_", "delete_", "add_", "apply_", "replace_", "drop_", "define_", "discard_",
        "load_", "export_", "set_", "update_",
    ];
    if DRAFT.iter().any(|p| name.starts_with(p)) {
        Group::Draft
    } else if name.starts_with("query_")
        || matches!(name, "clash" | "quantities" | "model_summary" | "page")
    {
        Group::Query
    } else {
        Group::Read
    }
}

fn group_icon(g: Group) -> &'static str {
    match g {
        Group::Read => "find",
        Group::Query => "query",
        Group::Draft => "diff",
    }
}

fn group_key(g: Group) -> &'static str {
    match g {
        Group::Read => "ai.step.group.read",
        Group::Query => "ai.step.group.query",
        Group::Draft => "ai.step.group.draft",
    }
}

/// Why a step failed, in words a person can act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    Auth,
    RateLimit,
    Context,
    NoTools,
    Blocked,
    Network,
    /// An HTTP status from the model service.
    Server(Option<u16>),
    Protocol,
    Timeout,
    NotFound,
    Invalid,
    Denied,
    Stale,
    /// The turn ended before the step could run.
    Skipped,
    Other,
}

fn http_code(lower: &str) -> Option<u16> {
    let at = lower.find("http status")? + "http status".len();
    let digits: String = lower[at..]
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok().filter(|c| (100..600).contains(c))
}

/// Maps a failure text (a provider error or a tool's own message) to a reason.
pub fn why(text: &str) -> Why {
    let t = text.trim().to_lowercase();
    let p = |s: &str| t.starts_with(s);
    let has = |s: &str| t.contains(s);
    if has("not run: the turn ended") {
        return Why::Skipped;
    }
    if p("authentication failed") {
        return Why::Auth;
    }
    if p("rate limited") {
        return Why::RateLimit;
    }
    if p("request exceeds the model context") {
        return Why::Context;
    }
    if p("model does not support tool calling") {
        return Why::NoTools;
    }
    if p("request blocked") {
        return Why::Blocked;
    }
    if p("network error") {
        return Why::Network;
    }
    if p("protocol error") {
        return Why::Protocol;
    }
    if let Some(code) = http_code(&t) {
        return match code {
            401 | 403 => Why::Auth,
            429 => Why::RateLimit,
            c => Why::Server(Some(c)),
        };
    }
    if p("http status") {
        return Why::Server(None);
    }
    if has("timed out") || has("timeout") {
        Why::Timeout
    } else if has("not found") || has("no such") || has("does not exist") || has("unknown element")
        || has("is gone") || has("no new element") || has("unknown tool")
    {
        Why::NotFound
    } else if has("not allowed") || has("denied") || has("read-only") || has("forbidden") {
        Why::Denied
    } else if has("stale") || has("was changed or replaced") {
        Why::Stale
    } else if has("invalid") || has("missing") || has("expected") || has("must be") {
        Why::Invalid
    } else if has("connection") || has("network") {
        Why::Network
    } else {
        Why::Other
    }
}

/// Longest piece of an unclassified failure text shown beside the generic reason.
const DETAIL_CHARS: usize = 160;

/// The reason line of a failed step: the classified reason, or, when none fits, the tool's own
/// text, so a failure is never reduced to "did not finish".
pub fn reason_line(cat: &Catalog, err: Option<&str>) -> String {
    let w = err.map_or(Why::Other, why);
    let base = why_text(cat, w);
    let detail = err.map(str::trim).filter(|e| !e.is_empty());
    match (w, detail) {
        (Why::Other, Some(d)) => {
            let short: String = d.chars().take(DETAIL_CHARS).collect();
            let dots = if d.chars().count() > DETAIL_CHARS { "\u{2026}" } else { "" };
            format!("{base}: {short}{dots}")
        }
        _ => base,
    }
}

pub fn why_text(cat: &Catalog, w: Why) -> String {
    let key = match w {
        Why::Auth => "ai.step.why.auth",
        Why::RateLimit => "ai.step.why.rate",
        Why::Context => "ai.step.why.context",
        Why::NoTools => "ai.step.why.no_tools",
        Why::Blocked => "ai.step.why.blocked",
        Why::Network => "ai.step.why.network",
        Why::Server(Some(code)) => {
            return cat.tf("ai.step.why.server", &[("code", &code.to_string())]);
        }
        Why::Server(None) => "ai.step.why.server_plain",
        Why::Protocol => "ai.step.why.protocol",
        Why::Timeout => "ai.step.why.timeout",
        Why::NotFound => "ai.step.why.not_found",
        Why::Invalid => "ai.step.why.invalid",
        Why::Denied => "ai.step.why.denied",
        Why::Stale => "ai.step.why.stale",
        Why::Skipped => "ai.step.why.skipped",
        Why::Other => "ai.step.why.other",
    };
    cat.t(key)
}

/// A duration: "412 ms", "3.2 s", "12 s", "1 min 5 s", with the locale's decimal sign.
pub fn fmt_dur(cat: &Catalog, ms: u64) -> String {
    let dec = cat.t("ai.step.dec");
    if ms < 1000 {
        return cat.tf("ai.step.ms", &[("n", &ms.to_string())]);
    }
    if ms < 10_000 {
        let t = (ms + 50) / 100;
        if t < 100 {
            let n = format!("{}{}{}", t / 10, dec, t % 10);
            return cat.tf("ai.step.s", &[("n", &n)]);
        }
    }
    if ms < 59_500 {
        return cat.tf("ai.step.s", &[("n", &((ms + 500) / 1000).to_string())]);
    }
    let total = (ms + 500) / 1000;
    let (m, s) = (total / 60, total % 60);
    if s == 0 {
        cat.tf("ai.step.min", &[("m", &m.to_string())])
    } else {
        cat.tf(
            "ai.step.min_s",
            &[("m", &m.to_string()), ("s", &s.to_string())],
        )
    }
}

/// What the card header needs to know about a run.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub steps: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub total_ms: u64,
    /// Name of the step that is still running.
    pub running: Option<String>,
    pub draft: bool,
}

pub fn run_summary(run: &[&Step], now_ms: u64) -> Summary {
    let mut s = Summary {
        steps: run.len(),
        failed: 0,
        cancelled: 0,
        total_ms: 0,
        running: None,
        draft: false,
    };
    for st in run {
        s.total_ms += st.duration(now_ms);
        s.draft |= st.group() == Group::Draft;
        match st.state() {
            State::Failed => s.failed += 1,
            State::Cancelled => s.cancelled += 1,
            State::Running => s.running = Some(st.name.clone()),
            State::Ok => {}
        }
    }
    s
}

pub fn title_key(s: &Summary) -> &'static str {
    if s.draft {
        "ai.step.title_draft"
    } else {
        "ai.step.title_read"
    }
}

/// "4 steps - 1 failed - 3.2 s", the muted half of the header.
pub fn summary_line(cat: &Catalog, s: &Summary) -> String {
    let mut parts = vec![cat.plural("ai.step.count", s.steps as u64)];
    if s.failed > 0 {
        parts.push(cat.tf("ai.step.failed", &[("n", &s.failed.to_string())]));
    }
    if s.cancelled > 0 {
        parts.push(cat.t("ai.step.stopped"));
    }
    if s.total_ms > 0 {
        parts.push(fmt_dur(cat, s.total_ms));
    }
    parts.join(&cat.t("ai.stream.sep"))
}

/// Header split of a row `width` wide after a `lead` (chevron): the summary takes at most
/// 60 % of the rest, the title what is left of it; both are elided, never overlapped.
pub fn header_widths(width: f32, lead: f32) -> (f32, f32) {
    let avail = (width - lead - space::SM).max(0.0);
    (avail, avail * 0.6)
}

fn state_key(s: State) -> &'static str {
    match s {
        State::Running => "ai.step.state.running",
        State::Ok => "ai.step.state.ok",
        State::Failed => "ai.step.state.failed",
        State::Cancelled => "ai.step.state.cancelled",
    }
}

fn glyph(ui: &mut Ui, th: &Theme, name: &str, col: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(th.d().icon), Sense::hover());
    let ctx = ui.ctx().clone();
    widgets::with_icons(&ctx, |icons| {
        icons.paint(ui, name, th.icon_px(), rect, col);
    });
    resp
}

fn state_glyph(ui: &mut Ui, cx: &Cx<'_>, state: State) {
    let (th, cat) = (cx.th, cx.cat);
    let resp = match state {
        State::Running => {
            kit::spinner(ui, th);
            return;
        }
        State::Ok => glyph(ui, th, "check", th.c("success")),
        State::Failed => glyph(ui, th, "warning", th.c("error")),
        State::Cancelled => glyph(ui, th, "close", th.c("text.disabled")),
    };
    resp.on_hover_text(cat.t(state_key(state)));
}

fn row(ui: &mut Ui, cx: &Cx<'_>, s: &Step, at: (usize, usize), now: u64) {
    let (th, cat) = (cx.th, cx.cat);
    let state = s.state();
    ui.horizontal(|ui| {
        state_glyph(ui, cx, state);
        glyph(ui, th, group_icon(s.group()), th.c("text.secondary"))
            .on_hover_text(cat.t(group_key(s.group())));
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            let dur = match state {
                State::Running | State::Ok | State::Failed => fmt_dur(cat, s.duration(now)),
                State::Cancelled => cat.t("ai.step.stopped"),
            };
            ui.label(
                RichText::new(dur)
                    .font(th.font(Role::Caption))
                    .color(th.c("text.disabled")),
            );
            ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                let col = if state == State::Failed {
                    th.c("text.primary")
                } else {
                    th.c("text.secondary")
                };
                widgets::elided_label(
                    ui,
                    &tool_name(cat, &s.name),
                    th.font(Role::Caption),
                    col,
                );
            });
        });
    });
    if state != State::Failed {
        return;
    }
    let reason = reason_line(cat, s.err.as_deref());
    let pad = th.d().icon + ui.spacing().item_spacing.x;
    ui.horizontal(|ui| {
        ui.add_space(pad);
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            if let Some(err) = &s.err {
                if widgets::glyph_button(ui, "copy", true, &cat.t("ai.step.copy")).clicked() {
                    ui.ctx().copy_text(err.clone());
                }
            }
            ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(reason)
                            .font(th.font(Role::Caption))
                            .color(th.c("text.secondary")),
                    )
                    .wrap(),
                );
            });
        });
    });
    if let Some(err) = &s.err {
        ui.horizontal(|ui| {
            ui.add_space(pad);
            egui::CollapsingHeader::new(
                RichText::new(cat.t("ai.step.details"))
                    .font(th.font(Role::Caption))
                    .color(th.c("text.disabled")),
            )
            .id_salt(("ddc.ai.step.details", at))
            .default_open(false)
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(err)
                            .font(th.font(Role::Code))
                            .color(th.c("text.secondary")),
                    )
                    .selectable(true)
                    .wrap(),
                );
            });
        });
    }
}

/// The card for one run of steps; `at` keeps its open state apart from other runs.
pub fn card(ui: &mut Ui, cx: &Cx<'_>, run: &[&Step], at: usize) {
    let (th, cat) = (cx.th, cx.cat);
    let now = now_ms();
    let sum = run_summary(run, now);
    let id = ui.id().with(("ddc.ai.steps", at));
    let live = sum.running.is_some();
    let mut open = ui
        .data(|d| d.get_temp::<bool>(id))
        .unwrap_or(sum.failed > 0 || live);
    ui.add_space(space::XS);
    kit::card(ui, th, |ui| {
        ui.horizontal(|ui| {
            if live {
                kit::spinner(ui, th);
            } else if sum.failed > 0 {
                glyph(ui, th, "warning", th.c("warning"));
            } else {
                glyph(ui, th, "check", th.c("success"));
            }
            // the header paints only the chevron; title and summary share its width here
            let head = widgets::section_header(ui, th, "", &mut open);
            let title = cat.t(title_key(&sum)).to_uppercase();
            let line = summary_line(cat, &sum);
            let (title_w, line_w) = header_widths(head.rect.width(), space::SM + space::MD);
            let r = head.rect;
            let right = r.right() - space::SM;
            let col = th.c("text.secondary");
            let g = widgets::elide(ui, &line, th.font(Role::Caption), col, line_w);
            let line_cut = g.elided;
            let gw = g.size().x;
            let pos = egui::pos2(right - gw, r.center().y - g.size().y / 2.0);
            ui.painter().galley(pos, g, col);
            let left = r.left() + space::SM + space::MD;
            let title_right = (right - gw - space::SM).min(left + title_w).max(left);
            let title_rect = egui::Rect::from_min_max(
                egui::pos2(left, r.top()),
                egui::pos2(title_right, r.bottom()),
            );
            let font = th.font(Role::Section);
            let title_cut =
                widgets::text::paint_in(ui, title_rect, &title, font, col, Align::Min);
            if line_cut || title_cut {
                head.on_hover_text(format!("{title}{}{line}", cat.t("ai.stream.sep")));
            }
        });
        if open {
            for (i, s) in run.iter().enumerate() {
                row(ui, cx, s, (at, i), now);
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(id, open));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::Lang;

    fn cat(l: Lang) -> Catalog {
        Catalog::new(l)
    }

    fn done(name: &str, ms: u32) -> Step {
        let mut s = Step::start(name, 1_000);
        s.finish(true, None, 1_000 + ms as u64);
        s
    }

    fn failed(name: &str, ms: u32, err: &str) -> Step {
        let mut s = Step::start(name, 1_000);
        s.finish(false, Some(err), 1_000 + ms as u64);
        s
    }

    #[test]
    fn the_four_states() {
        let mut s = Step::start("get_element", 100);
        assert_eq!(s.state(), State::Running);
        assert_eq!(s.duration(350), 250, "a running step counts live");
        s.finish(true, None, 400);
        assert_eq!((s.state(), s.ms), (State::Ok, Some(300)));
        assert_eq!(s.duration(9_999), 300, "a finished step stops counting");
        assert_eq!(failed("clash", 5, "boom").state(), State::Failed);
        let mut c = Step::start("clash", 100);
        c.stop(None, 160);
        assert_eq!((c.state(), c.ms, c.ok), (State::Cancelled, Some(60), None));
        c.stop(Some("late"), 900);
        assert_eq!(c.state(), State::Cancelled, "a closed step stays as it was");
    }

    #[test]
    fn a_failure_ends_a_running_step() {
        let mut s = Step::start("query_elements", 0);
        s.stop(Some("HTTP status 503 Service Unavailable"), 40);
        assert_eq!(s.state(), State::Failed);
        assert_eq!(why(s.err.as_deref().unwrap()), Why::Server(Some(503)));
    }

    #[test]
    fn a_clock_going_back_never_panics() {
        let mut s = Step::start("page", 500);
        s.finish(true, None, 100);
        assert_eq!(s.ms, Some(0));
    }

    #[test]
    fn durations_ms_s_min_in_both_languages() {
        let en = cat(Lang::En);
        let ru = cat(Lang::Ru);
        assert_eq!(fmt_dur(&en, 0), "0 ms");
        assert_eq!(fmt_dur(&en, 412), "412 ms");
        assert_eq!(fmt_dur(&en, 999), "999 ms");
        assert_eq!(fmt_dur(&en, 1_000), "1.0 s");
        assert_eq!(fmt_dur(&en, 3_240), "3.2 s");
        assert_eq!(fmt_dur(&en, 9_960), "10 s");
        assert_eq!(fmt_dur(&en, 12_400), "12 s");
        assert_eq!(fmt_dur(&en, 59_499), "59 s");
        assert_eq!(fmt_dur(&en, 59_500), "1 min");
        assert_eq!(fmt_dur(&en, 65_000), "1 min 5 s");
        assert_eq!(fmt_dur(&en, 600_000), "10 min");
        assert_eq!(fmt_dur(&ru, 412), crate::app2_fx::fx("dur_412"));
        assert_eq!(fmt_dur(&ru, 3_240), crate::app2_fx::fx("dur_3240"));
        assert_eq!(fmt_dur(&ru, 65_000), crate::app2_fx::fx("dur_65000"));
    }

    #[test]
    fn reasons_for_http_provider_and_tool_errors() {
        assert_eq!(why("HTTP status 502 Bad Gateway"), Why::Server(Some(502)));
        assert_eq!(why("http status 429"), Why::RateLimit);
        assert_eq!(why("http status 401"), Why::Auth);
        assert_eq!(why("http status"), Why::Server(None));
        assert_eq!(why("authentication failed: bad key"), Why::Auth);
        assert_eq!(why("rate limited, retry after 5 s"), Why::RateLimit);
        assert_eq!(why("network error: dns"), Why::Network);
        assert_eq!(why("request exceeds the model context"), Why::Context);
        assert_eq!(why("protocol error: bad frame"), Why::Protocol);
        assert_eq!(why("[not run: the turn ended]"), Why::Skipped);
        assert_eq!(why("element 12 not found"), Why::NotFound);
        // the plan's own words for a reference the model invented or lost
        assert_eq!(why(r#"{"error":"element is gone: wall:999"}"#), Why::NotFound);
        assert_eq!(why("no new element n9 in this plan"), Why::NotFound);
        assert_eq!(why("element was changed or replaced: door:5 is a window"), Why::Stale);
        assert_eq!(why("the tool timed out"), Why::Timeout);
        assert_eq!(why("invalid filter: expected a category"), Why::Invalid);
        assert_eq!(why("the model is read-only"), Why::Denied);
        assert_eq!(why("something odd"), Why::Other);
        assert_eq!(why("unknown tool: view_measure"), Why::NotFound);
        let c = cat(Lang::En);
        assert!(reason_line(&c, Some("door GF-D06 hit the frame")).ends_with(": door GF-D06 hit the frame"));
        assert_eq!(reason_line(&c, None), why_text(&c, Why::Other));
        assert_eq!(why(""), Why::Other);
    }

    #[test]
    fn reason_text_exists_and_differs_per_language() {
        let all = [
            Why::Auth,
            Why::RateLimit,
            Why::Context,
            Why::NoTools,
            Why::Blocked,
            Why::Network,
            Why::Server(Some(503)),
            Why::Server(None),
            Why::Protocol,
            Why::Timeout,
            Why::NotFound,
            Why::Invalid,
            Why::Denied,
            Why::Stale,
            Why::Skipped,
            Why::Other,
        ];
        for w in all {
            let (en, ru) = (why_text(&cat(Lang::En), w), why_text(&cat(Lang::Ru), w));
            assert!(!en.starts_with("ai.step"), "{w:?}: {en}");
            assert!(!ru.starts_with("ai.step"), "{w:?}: {ru}");
            assert_ne!(en, ru, "{w:?}");
        }
        assert!(why_text(&cat(Lang::En), Why::Server(Some(503))).contains("503"));
    }

    #[test]
    fn long_text_is_one_masked_line_with_an_ellipsis() {
        let long = format!("bad arguments\n{}", "x ".repeat(500));
        let c = clean_detail(&long).unwrap();
        assert_eq!(c.chars().count(), DETAIL_MAX);
        assert!(c.ends_with('\u{2026}') && !c.contains('\n'));
        let multi = format!("{}\u{2026}", "\u{4e2d}".repeat(400));
        assert!(elide_chars(&multi, 10).chars().count() == 10, "char safe");
        assert_eq!(elide_chars("short", 10), "short");
        assert_eq!(clean_detail("  \n\t "), None);
        let key = clean_detail("failed with Bearer abcdefghijklmnop1234567890").unwrap();
        assert!(!key.contains("abcdefghijklmnop1234567890"), "{key}");
    }

    #[test]
    fn a_step_keeps_a_capped_reason() {
        let s = failed("clash", 9, &"y ".repeat(2_500));
        assert_eq!(s.err.as_ref().unwrap().chars().count(), DETAIL_MAX);
        let ok = done("clash", 9);
        assert_eq!(ok.err, None);
    }

    #[test]
    fn summary_counts_steps_failures_and_time() {
        let a = done("get_context", 400);
        let b = failed("query_elements", 800, "element 3 not found");
        let c = done("create_elements", 2_000);
        let r = run_summary(&[&a, &b, &c], 99_999);
        assert_eq!((r.steps, r.failed, r.cancelled), (3, 1, 0));
        assert_eq!(r.total_ms, 3_200);
        assert_eq!(r.running, None);
        assert!(r.draft);
        assert_eq!(title_key(&r), "ai.step.title_draft");
        let line = summary_line(&cat(Lang::En), &r);
        assert_eq!(line, "3 steps \u{b7} 1 failed \u{b7} 3.2 s");
    }

    #[test]
    fn summary_names_the_running_step_and_counts_it_live() {
        let a = done("get_context", 500);
        let b = Step::start("model_summary", 5_000);
        let r = run_summary(&[&a, &b], 6_500);
        assert_eq!(r.running.as_deref(), Some("model_summary"));
        assert_eq!(r.total_ms, 2_000);
        assert_eq!(title_key(&r), "ai.step.title_read");
        let mut c = Step::start("clash", 0);
        c.stop(None, 10);
        let r = run_summary(&[&c], 10);
        assert_eq!(r.cancelled, 1);
        assert!(summary_line(&cat(Lang::En), &r).contains("Stopped"));
        assert!(summary_line(&cat(Lang::Ru), &r).contains(&crate::app2_fx::fx("steps_stopped")));
    }

    #[test]
    fn step_count_has_russian_plural_forms() {
        let ru = cat(Lang::Ru);
        assert_eq!(ru.plural("ai.step.count", 1), crate::app2_fx::fx("step_1"));
        assert_eq!(ru.plural("ai.step.count", 3), crate::app2_fx::fx("step_3"));
        assert_eq!(ru.plural("ai.step.count", 5), crate::app2_fx::fx("step_5"));
        let en = cat(Lang::En);
        assert_eq!(en.plural("ai.step.count", 1), "1 step");
        assert_eq!(en.plural("ai.step.count", 4), "4 steps");
    }

    #[test]
    fn groups_split_read_query_and_draft() {
        assert_eq!(group_of("get_element"), Group::Read);
        assert_eq!(group_of("list_types"), Group::Read);
        assert_eq!(group_of("query_elements"), Group::Query);
        assert_eq!(group_of("clash"), Group::Query);
        assert_eq!(group_of("create_elements"), Group::Draft);
        assert_eq!(group_of("replace_type"), Group::Draft);
        assert_eq!(group_of("unknown_tool"), Group::Read);
    }

    #[test]
    fn rows_round_trip_with_the_reason() {
        let f = failed("query_elements", 800, "HTTP status 503");
        let back = Step::from_row(&f.to_row()).unwrap();
        assert_eq!(back.state(), State::Failed);
        assert_eq!(back.err, f.err);
        assert_eq!(back.ms, Some(800));
        let ok = Step::from_row(&done("get_context", 12).to_row()).unwrap();
        assert_eq!((ok.state(), ok.ms, ok.err), (State::Ok, Some(12), None));
        let mut c = Step::start("clash", 0);
        c.stop(None, 33);
        assert_eq!(Step::from_row(&c.to_row()).unwrap().state(), State::Cancelled);
    }

    #[test]
    fn old_and_odd_rows_load_or_are_dropped() {
        let old = Step::from_row(r#"{"n":"query_elements","ok":true}"#).unwrap();
        assert_eq!((old.state(), old.ms), (State::Ok, None));
        let bare = Step::from_row(r#"{"n":"page"}"#).unwrap();
        assert_eq!(bare.state(), State::Cancelled, "never running after a reload");
        let leaked = Step::from_row(r#"{"n":"page","ok":true,"e":"secret text"}"#).unwrap();
        assert_eq!(leaked.err, None, "only a failed step has a reason");
        assert_eq!(Step::from_row("not json"), None);
        assert_eq!(Step::from_row(r#"{"ok":true}"#), None);
        assert_eq!(Step::from_row(r#"{"n":"Bad Name"}"#), None);
        assert_eq!(Step::from_row(&format!(r#"{{"n":"{}"}}"#, "a".repeat(80))), None);
    }

    #[test]
    fn unknown_tool_names_are_generic() {
        let en = cat(Lang::En);
        let generic = en.t("ai.step.unknown");
        assert_eq!(tool_name(&en, "unknown_tool"), generic);
        assert_eq!(tool_name(&en, "drop_all_tables_now"), generic);
        assert_ne!(tool_name(&en, "query_elements"), generic);
        assert!(!tool_name(&en, "query_elements").starts_with("ai."));
    }

    #[test]
    fn a_long_run_keeps_the_latest_failures_and_steps() {
        let steps: Vec<Step> = (0..40)
            .map(|i| if i % 10 == 0 { failed("clash", 1, "x") } else { done("page", 1) })
            .collect();
        let run: Vec<&Step> = steps.iter().collect();
        let kept = saved(&run);
        assert_eq!(kept.len(), SAVED_PER_RUN);
        assert!([0, 10, 20, 30].iter().all(|i| kept.contains(i)), "{kept:?}");
        assert!(kept.windows(2).all(|w| w[0] < w[1]), "order kept");
        assert_eq!(*kept.last().unwrap(), 39);
        assert_eq!(saved(&run[..5]), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn header_split_never_overlaps_or_goes_negative() {
        for w in [0.0, 40.0, 180.0, 900.0] {
            let (title, line) = header_widths(w, 20.0);
            assert!(title >= 0.0 && line >= 0.0);
            assert!(line <= title * 0.6 + 0.01 && title + 20.0 <= w.max(20.0 + space::SM));
        }
    }

    #[test]
    fn every_key_exists_in_en_and_ru() {
        let mut keys = vec![
            "ai.step.unknown",
            "ai.step.title_read",
            "ai.step.title_draft",
            "ai.step.failed",
            "ai.step.stopped",
            "ai.step.ms",
            "ai.step.s",
            "ai.step.min",
            "ai.step.min_s",
            "ai.step.dec",
            "ai.step.details",
            "ai.step.copy",
            "ai.step.group.read",
            "ai.step.group.query",
            "ai.step.group.draft",
            "ai.step.state.running",
            "ai.step.state.ok",
            "ai.step.state.failed",
            "ai.step.state.cancelled",
            "ai.step.why.server_plain",
        ];
        for w in [
            "auth", "rate", "context", "no_tools", "blocked", "network", "server", "protocol",
            "timeout", "not_found", "invalid", "denied", "stale", "skipped", "other",
        ] {
            keys.push(Box::leak(format!("ai.step.why.{w}").into_boxed_str()));
        }
        for k in keys {
            for l in [Lang::En, Lang::Ru] {
                assert_ne!(crate::tr_in(l, k, &[]), k, "{k} in {l:?}");
            }
        }
    }
}
