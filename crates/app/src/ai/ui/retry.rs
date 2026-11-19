// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Automatic recovery of a failed turn: a countdown card, then the same prompt and mode again.
//! Only the turn is resent. A draft is never touched here and nothing is applied: the model
//! changes only when the person presses Apply.

use super::kit::{self, space, Button};
use super::logic::{self, ErrKind};
use super::{Action, Cx};
use crate::ai::AiError;
use egui::{RichText, Ui};
use ddc_ui::theme::Role;

/// Automatic resends per prompt.
pub const MAX_TRIES: u32 = 3;
/// Waits in seconds when the provider names none; index = resends already made.
pub const BACKOFF_S: [u32; 3] = [2, 4, 8];
/// A provider wait is kept within this many seconds.
pub const WAIT_MAX_S: u32 = 120;

/// Failures that pass by themselves: busy, throttled or unreachable.
pub const fn retryable(k: ErrKind) -> bool {
    matches!(k, ErrKind::RateLimit | ErrKind::Network | ErrKind::Server)
}

/// The kind used for automatic resends: an HTTP failure counts as `Server` only for 5xx;
/// a request the server refused (4xx) fails the same way again.
pub fn auto_kind(e: &AiError) -> ErrKind {
    match (logic::classify(e), logic::error_detail(e).status) {
        (ErrKind::Server, Some(500..=599)) => ErrKind::Server,
        (ErrKind::Server, _) => ErrKind::Internal,
        (k, _) => k,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Resend after this many seconds.
    Wait(u32),
    GiveUp,
}

/// `attempt`: automatic resends already made for this prompt.
pub fn retry_plan(kind: ErrKind, retry_after: Option<u32>, attempt: u32) -> Next {
    if !retryable(kind) || attempt >= MAX_TRIES {
        return Next::GiveUp;
    }
    let s = retry_after.unwrap_or(BACKOFF_S[attempt as usize]);
    Next::Wait(s.clamp(1, WAIT_MAX_S))
}

/// The one fix button of a failure that no resend can cure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fix {
    Settings,
    Shorten,
}

pub const fn fix_for(k: ErrKind) -> Option<Fix> {
    match k {
        ErrKind::Auth | ErrKind::Blocked | ErrKind::NoTools => Some(Fix::Settings),
        ErrKind::Context => Some(Fix::Shorten),
        _ => None,
    }
}

pub const fn fix_label_key(f: Fix) -> &'static str {
    match f {
        Fix::Settings => "cmd.ai.settings",
        Fix::Shorten => "ai.retry.shorten",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Idle,
    /// The countdown ended: resend now.
    Fire,
}

#[derive(Default, Debug, Clone)]
pub struct RetryState {
    attempt: u32,
    due: Option<f64>,
    /// The current error was already looked at.
    handled: bool,
    /// The person cancelled; no more automatic resends for this prompt.
    stopped: bool,
    /// All automatic resends were spent.
    exhausted: bool,
}

impl RetryState {
    /// A new prompt or a manual retry starts over.
    pub fn reset(&mut self) {
        *self = RetryState::default();
    }

    /// Call every frame with the current error. `now` is a clock in seconds.
    pub fn step(
        &mut self,
        kind: Option<ErrKind>,
        retry_after: Option<u32>,
        has_draft: bool,
        now: f64,
    ) -> Step {
        let Some(kind) = kind else {
            self.handled = false;
            self.due = None;
            return Step::Idle;
        };
        if !self.handled {
            self.handled = true;
            self.due = None;
            self.exhausted = false;
            if has_draft || self.stopped {
                return Step::Idle;
            }
            match retry_plan(kind, retry_after, self.attempt) {
                Next::Wait(s) => {
                    self.due = Some(now + f64::from(s));
                    self.attempt += 1;
                }
                Next::GiveUp => self.exhausted = self.attempt > 0 && retryable(kind),
            }
            return Step::Idle;
        }
        match self.due {
            Some(d) if now >= d => {
                self.due = None;
                self.handled = false;
                Step::Fire
            }
            _ => Step::Idle,
        }
    }

    /// Whole seconds left, rounded up; `None` when nothing is counting down.
    pub fn remaining(&self, now: f64) -> Option<u32> {
        let d = self.due?;
        Some((d - now).max(0.0).ceil() as u32)
    }

    /// "Retry now": skip the rest of the wait, keep the count of tries.
    pub fn retry_now(&mut self) {
        self.due = None;
        self.handled = false;
    }

    /// "Cancel": stop counting and automatic resends; the plain error card stays.
    pub fn cancel(&mut self) {
        self.due = None;
        self.stopped = true;
    }

    /// 1-based number of the resend that is counting down or was last scheduled.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Show the closing note: every automatic try failed.
    pub fn exhausted(&self) -> bool {
        self.exhausted
    }
}

/// Catalog keys and the number for the countdown line.
pub fn countdown_parts(remaining: u32) -> (&'static str, String) {
    ("ai.retry.countdown", remaining.to_string())
}

/// The countdown card; true when it was drawn (the plain error card is skipped).
pub fn card(ui: &mut Ui, cx: &Cx<'_>, st: &RetryState, acts: &mut Vec<Action>) -> bool {
    let now = ui.ctx().input(|i| i.time);
    let Some(rem) = st.remaining(now) else {
        return false;
    };
    let (th, cat) = (cx.th, cx.cat);
    let (key, n) = countdown_parts(rem);
    ui.add_space(space::SM);
    kit::card(ui, th, |ui| {
        ui.label(
            RichText::new(cat.tf(key, &[("n", &n)]))
                .font(th.font(Role::Label))
                .color(th.c("text.primary")),
        );
        kit::caption(
            ui,
            th,
            &cat.tf(
                "ai.retry.try",
                &[("i", &st.attempt().to_string()), ("max", &MAX_TRIES.to_string())],
            ),
        );
        ui.horizontal(|ui| {
            if Button::secondary(&cat.t("ai.retry.now")).small().show(ui, th).clicked() {
                acts.push(Action::RetryNow);
            }
            if Button::quiet(&cat.t("ai.common.cancel")).small().show(ui, th).clicked() {
                acts.push(Action::RetryCancel);
            }
        });
    });
    true
}

/// The note above the plain error card after the last automatic try.
pub fn exhausted_note(ui: &mut Ui, cx: &Cx<'_>) {
    ui.add_space(space::SM);
    kit::caption(
        ui,
        cx.th,
        &cx.cat.tf("ai.retry.gave_up", &[("max", &MAX_TRIES.to_string())]),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::AiError;
    use ddc_ui::{Catalog, Lang};

    use super::super::logic::classify;

    fn prov(t: &str) -> AiError {
        AiError::Provider(t.to_string())
    }

    #[test]
    fn classification_per_kind() {
        let cases = [
            ("rate limited (retry after 7 s)", ErrKind::RateLimit, true),
            ("http status 503: overloaded", ErrKind::Server, true),
            ("network error: refused", ErrKind::Network, true),
            ("authentication failed", ErrKind::Auth, false),
            ("request blocked: bad host", ErrKind::Blocked, false),
            ("request exceeds the model context", ErrKind::Context, false),
            ("model does not support tool calling", ErrKind::NoTools, false),
            ("protocol error: junk", ErrKind::Protocol, false),
        ];
        for (text, kind, auto) in cases {
            let k = classify(&prov(text));
            assert_eq!(k, kind, "{text}");
            assert_eq!(retryable(k), auto, "{text}");
        }
        assert!(!retryable(ErrKind::Stale));
        assert!(!retryable(ErrKind::Disk));
        assert!(!retryable(ErrKind::Model));
    }

    #[test]
    fn only_server_side_http_codes_are_resent() {
        assert!(!retryable(auto_kind(&prov("http status 400: bad request"))));
        assert!(!retryable(auto_kind(&prov("http status 404: no such model"))));
        assert!(!retryable(auto_kind(&prov("http status 422: invalid"))));
        assert!(retryable(auto_kind(&prov("http status 502: bad gateway"))));
        assert!(retryable(auto_kind(&prov("http status 500: x"))));
        assert!(retryable(auto_kind(&prov("network error: refused"))));
    }

    #[test]
    fn fix_buttons() {
        assert_eq!(fix_for(ErrKind::Auth), Some(Fix::Settings));
        assert_eq!(fix_for(ErrKind::Blocked), Some(Fix::Settings));
        assert_eq!(fix_for(ErrKind::Context), Some(Fix::Shorten));
        assert_eq!(fix_for(ErrKind::Network), None);
        for k in [ErrKind::Auth, ErrKind::Blocked, ErrKind::Context] {
            assert!(!retryable(k));
        }
    }

    #[test]
    fn backoff_schedule_without_retry_after() {
        let k = ErrKind::Network;
        assert_eq!(retry_plan(k, None, 0), Next::Wait(2));
        assert_eq!(retry_plan(k, None, 1), Next::Wait(4));
        assert_eq!(retry_plan(k, None, 2), Next::Wait(8));
        assert_eq!(retry_plan(k, None, 3), Next::GiveUp);
    }

    #[test]
    fn retry_after_wins_and_is_bounded() {
        let k = ErrKind::RateLimit;
        assert_eq!(retry_plan(k, Some(12), 0), Next::Wait(12));
        assert_eq!(retry_plan(k, Some(12), 2), Next::Wait(12));
        assert_eq!(retry_plan(k, Some(0), 0), Next::Wait(1));
        assert_eq!(retry_plan(k, Some(9999), 0), Next::Wait(WAIT_MAX_S));
        assert_eq!(retry_plan(k, Some(12), 3), Next::GiveUp);
    }

    #[test]
    fn never_waits_for_other_kinds() {
        for k in [ErrKind::Auth, ErrKind::Blocked, ErrKind::Context, ErrKind::Stale] {
            assert_eq!(retry_plan(k, Some(5), 0), Next::GiveUp);
        }
    }

    /// Fails `n` times in a row on a fake clock; returns the waits seen and the final state.
    fn run_fails(n: usize, ra: Option<u32>) -> (Vec<u32>, RetryState) {
        let mut st = RetryState::default();
        let mut now = 100.0;
        let mut waits = vec![];
        for _ in 0..n {
            assert_eq!(st.step(Some(ErrKind::Network), ra, false, now), Step::Idle);
            let Some(r) = st.remaining(now) else { break };
            waits.push(r);
            now += f64::from(r) - 0.5;
            assert_eq!(st.step(Some(ErrKind::Network), ra, false, now), Step::Idle);
            now += 0.5;
            assert_eq!(st.step(Some(ErrKind::Network), ra, false, now), Step::Fire);
            // the resent turn failed again: the error is back
        }
        (waits, st)
    }

    #[test]
    fn three_tries_then_final_card() {
        let (waits, mut st) = run_fails(4, None);
        assert_eq!(waits, vec![2, 4, 8]);
        let now = 200.0;
        assert_eq!(st.step(Some(ErrKind::Network), None, false, now), Step::Idle);
        assert!(st.remaining(now).is_none());
        assert!(st.exhausted());
        assert_eq!(st.step(Some(ErrKind::Network), None, false, now + 99.0), Step::Idle);
    }

    #[test]
    fn retry_after_sets_each_wait() {
        let (waits, _) = run_fails(3, Some(12));
        assert_eq!(waits, vec![12, 12, 12]);
    }

    #[test]
    fn countdown_with_fake_clock() {
        let mut st = RetryState::default();
        st.step(Some(ErrKind::RateLimit), Some(12), false, 50.0);
        assert_eq!(st.remaining(50.0), Some(12));
        assert_eq!(st.remaining(50.2), Some(12));
        assert_eq!(st.remaining(55.0), Some(7));
        assert_eq!(st.remaining(61.5), Some(1));
        assert_eq!(st.remaining(70.0), Some(0));
        let (key, n) = countdown_parts(12);
        assert_eq!((key, n.as_str()), ("ai.retry.countdown", "12"));
        let en = Catalog::new(Lang::En);
        assert_eq!(en.tf(key, &[("n", "12")]), "Retrying in 12 s");
        let ru = Catalog::new(Lang::Ru);
        let t = ru.tf(key, &[("n", "12")]);
        assert!(t.contains("12") && t != en.tf(key, &[("n", "12")]));
    }

    #[test]
    fn cancel_stops_everything() {
        let mut st = RetryState::default();
        st.step(Some(ErrKind::Network), None, false, 0.0);
        assert!(st.remaining(0.0).is_some());
        st.cancel();
        assert!(st.remaining(0.0).is_none());
        assert_eq!(st.step(Some(ErrKind::Network), None, false, 100.0), Step::Idle);
        // the next failure of the same prompt does not count down either
        st.step(None, None, false, 101.0);
        st.step(Some(ErrKind::Network), None, false, 102.0);
        assert!(st.remaining(102.0).is_none());
        assert!(!st.exhausted());
        // a new prompt starts over
        st.reset();
        st.step(Some(ErrKind::Network), None, false, 103.0);
        assert_eq!(st.remaining(103.0), Some(2));
    }

    #[test]
    fn retry_now_skips_the_wait_and_keeps_the_count() {
        let mut st = RetryState::default();
        st.step(Some(ErrKind::Server), None, false, 0.0);
        st.retry_now();
        assert!(st.remaining(0.5).is_none());
        // error cleared by the resend, then it fails again: second wait of the schedule
        st.step(None, None, false, 0.6);
        st.step(Some(ErrKind::Server), None, false, 1.0);
        assert_eq!(st.remaining(1.0), Some(4));
        assert_eq!(st.attempt(), 2);
    }

    #[test]
    fn no_retry_once_a_draft_exists() {
        let mut st = RetryState::default();
        assert_eq!(st.step(Some(ErrKind::Network), None, true, 0.0), Step::Idle);
        assert!(st.remaining(0.0).is_none());
        assert_eq!(st.attempt(), 0);
        assert_eq!(st.step(Some(ErrKind::Network), None, true, 50.0), Step::Idle);
    }

    #[test]
    fn non_retryable_errors_show_no_countdown() {
        for k in [ErrKind::Auth, ErrKind::Blocked, ErrKind::Context] {
            let mut st = RetryState::default();
            st.step(Some(k), None, false, 0.0);
            assert!(st.remaining(0.0).is_none());
            assert!(!st.exhausted());
        }
    }

    #[test]
    fn strings_exist_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            for key in [
                "ai.retry.countdown",
                "ai.retry.now",
                "ai.retry.try",
                "ai.retry.gave_up",
                "ai.retry.shorten",
            ] {
                assert!(c.table(lang).contains_key(key), "{key}");
            }
        }
    }
}
