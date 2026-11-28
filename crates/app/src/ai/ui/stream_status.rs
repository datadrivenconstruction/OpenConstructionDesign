// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! What the assistant is doing right now, as one status row. The panel records timestamps
//! only; `stream_status` turns them into the row and the view just draws it.

use web_time::{Duration, Instant};

/// No data for this long means the model service is not answering.
pub const STALL_AFTER: Duration = Duration::from_secs(20);
/// The timer shows whole seconds; a few repaints a second are enough.
pub const TICK: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq)]
pub enum Ev {
    /// The turn was sent.
    Start(Instant),
    /// A tool call began (`name`) or ended.
    ToolStart(Instant, String),
    ToolEnd(Instant),
    /// An answer fragment arrived.
    Text(Instant),
    /// An empty fragment: the connection is alive, nothing to show.
    Heartbeat(Instant),
    Done(Instant),
    /// The person stopped the turn.
    Cancel(Instant),
    /// Tokens the model has written since the last tool event (answer text or tool-call
    /// arguments): `first` and `last` bound them in time. Never more than one in the log.
    Tokens { first: Instant, last: Instant, n: u64 },
    /// This computer builds the draft: `done` of `total` elements.
    Build(Instant, u64, u64),
}

impl Ev {
    fn at(&self) -> Instant {
        match self {
            Ev::Start(t)
            | Ev::ToolStart(t, _)
            | Ev::ToolEnd(t)
            | Ev::Text(t)
            | Ev::Heartbeat(t)
            | Ev::Done(t)
            | Ev::Cancel(t)
            | Ev::Build(t, _, _) => *t,
            Ev::Tokens { last, .. } => *last,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// Sent, nothing back yet.
    Waiting,
    /// A tool is running.
    Reading,
    /// Between steps.
    Working,
    Answering,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StatusRow {
    pub phase: Phase,
    pub tool: Option<String>,
    pub tools: usize,
    pub secs: u64,
    pub caret: bool,
    pub stalled: bool,
    /// Whole seconds since the last sign of life.
    pub quiet_secs: u64,
    /// A tool runs here, not at the model service, and has been silent for a while.
    pub local_busy: bool,
    /// The turn was stopped before it finished: any draft is partial and nothing was applied.
    pub incomplete: bool,
    /// Tokens written since the last tool event and the speed over that stretch.
    pub written: u64,
    pub rate: Option<u64>,
    /// The draft is being built here: elements done of all.
    pub build: Option<(u64, u64)>,
}

impl StatusRow {
    pub fn idle() -> StatusRow {
        StatusRow {
            phase: Phase::Idle,
            tool: None,
            tools: 0,
            secs: 0,
            caret: false,
            stalled: false,
            quiet_secs: 0,
            local_busy: false,
            incomplete: false,
            written: 0,
            rate: None,
            build: None,
        }
    }

    /// Only a live turn needs frames.
    pub fn live(&self) -> bool {
        !matches!(self.phase, Phase::Idle | Phase::Cancelled)
    }
}

/// Events of the current turn (from its `Start`) and the clock in, the row out.
pub fn stream_status(events: &[Ev], now: Instant) -> StatusRow {
    let Some(start) = events.iter().rposition(|e| matches!(e, Ev::Start(_))) else {
        return StatusRow::idle();
    };
    let turn = &events[start..];
    let began = turn[0].at();
    let mut row = StatusRow::idle();
    row.phase = Phase::Waiting;
    let mut last = began;
    let mut end: Option<Instant> = None;
    for e in &turn[1..] {
        last = last.max(e.at());
        if !matches!(e, Ev::Build(..)) {
            row.build = None;
        }
        match e {
            Ev::ToolStart(_, name) => {
                row.tools += 1;
                row.tool = Some(name.clone());
                row.phase = Phase::Reading;
            }
            Ev::ToolEnd(_) => {
                row.tool = None;
                row.phase = Phase::Working;
            }
            Ev::Text(_) => {
                row.tool = None;
                row.phase = Phase::Answering;
            }
            Ev::Heartbeat(_) | Ev::Start(_) => {}
            Ev::Tokens { first, last: l, n } => {
                row.written = *n;
                let span = l.saturating_duration_since(*first).as_secs_f64();
                // under a second of data gives no honest speed
                row.rate = (span >= 1.0).then(|| (*n as f64 / span).round() as u64);
                if row.phase == Phase::Waiting || row.phase == Phase::Working {
                    row.phase = Phase::Answering;
                }
            }
            Ev::Build(_, done, total) => row.build = Some((*done, *total)),
            Ev::Done(t) => {
                end = Some(*t);
                row.phase = Phase::Idle;
                row.tool = None;
            }
            Ev::Cancel(t) => {
                end = Some(*t);
                row.phase = Phase::Cancelled;
                row.incomplete = true;
                row.tool = None;
            }
        }
        if end.is_some() {
            break;
        }
    }
    let upto = end.unwrap_or(now);
    row.secs = upto.saturating_duration_since(began).as_secs();
    if row.live() {
        let quiet = now.saturating_duration_since(last);
        row.quiet_secs = quiet.as_secs();
        // a running tool is this computer's work: the service is not the one that is slow
        let long = quiet >= STALL_AFTER;
        row.local_busy = long && row.phase == Phase::Reading;
        row.stalled = long && !row.local_busy;
        row.caret = row.phase == Phase::Answering && !row.stalled;
    }
    row
}

/// When to draw next: never while nothing streams, so an idle panel costs no frames.
pub fn repaint_after(row: &StatusRow) -> Option<Duration> {
    row.live().then_some(TICK)
}

/// Keeps the log short: runs of answer fragments and heartbeats fold into the last one.
pub fn push(events: &mut Vec<Ev>, e: Ev) {
    if matches!(e, Ev::Start(_)) {
        events.clear();
    }
    // the count belongs to one stretch of writing: a tool event closes it
    if matches!(e, Ev::ToolStart(..) | Ev::ToolEnd(_)) {
        events.retain(|x| !matches!(x, Ev::Tokens { .. }));
    }
    if let Ev::Tokens { first, last, n } = e {
        if let Some(Ev::Tokens { last: l, n: m, .. }) =
            events.iter_mut().rev().find(|x| matches!(x, Ev::Tokens { .. }))
        {
            *l = last;
            *m += n;
            return;
        }
        events.push(Ev::Tokens { first, last, n });
        return;
    }
    if let Ev::Build(..) = e {
        events.retain(|x| !matches!(x, Ev::Build(..)));
    }
    if let (Some(last), Ev::Text(_) | Ev::Heartbeat(_)) = (events.last_mut(), &e) {
        if matches!(last, Ev::Text(_) | Ev::Heartbeat(_)) {
            // a heartbeat after text must not turn the phase back to waiting
            let text = matches!(last, Ev::Text(_)) || matches!(e, Ev::Text(_));
            let t = e.at();
            *last = if text { Ev::Text(t) } else { Ev::Heartbeat(t) };
            return;
        }
    }
    events.push(e);
}

fn fmt_count(n: u64) -> String {
    if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// The row as one line: "Reading the model - 3 tools - 12 s".
pub fn row_text(row: &StatusRow, tr: &dyn Fn(&str, &[(&str, &str)]) -> String) -> String {
    if row.stalled {
        let n = row.quiet_secs.to_string();
        let total = tr("ai.stream.secs", &[("n", &row.secs.to_string())]);
        return [tr("ai.stream.stalled", &[("n", &n)]), total].join(&tr("ai.stream.sep", &[]));
    }
    let phase = match row.phase {
        Phase::Idle => return String::new(),
        Phase::Cancelled => return tr("ai.stream.cancelled", &[]),
        Phase::Waiting => "ai.stream.waiting",
        Phase::Reading => "ai.stream.reading",
        Phase::Working => "ai.stream.working",
        Phase::Answering => "ai.stream.answering",
    };
    if let Some((done, total)) = row.build {
        let (d, t) = (done.to_string(), total.to_string());
        let secs = tr("ai.stream.secs", &[("n", &row.secs.to_string())]);
        return [tr("ai.stream.building", &[("done", &d), ("total", &t)]), secs]
            .join(&tr("ai.stream.sep", &[]));
    }
    if row.phase == Phase::Answering && row.written > 0 {
        let mut parts = vec![tr("ai.stream.writing", &[("n", &fmt_count(row.written))])];
        if let Some(r) = row.rate {
            parts.push(tr("ai.stream.rate", &[("n", &r.to_string())]));
        }
        parts.push(tr("ai.stream.secs", &[("n", &row.secs.to_string())]));
        return parts.join(&tr("ai.stream.sep", &[]));
    }
    let mut parts = vec![tr(if row.local_busy { "ai.stream.local" } else { phase }, &[])];
    match row.tools {
        0 => {}
        1 => parts.push(tr("ai.stream.tools_one", &[])),
        n => parts.push(tr("ai.stream.tools_many", &[("n", &n.to_string())])),
    }
    parts.push(tr("ai.stream.secs", &[("n", &row.secs.to_string())]));
    parts.join(&tr("ai.stream.sep", &[]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::i18n::Lang;

    fn at(base: Instant, s: u64) -> Instant {
        base + Duration::from_secs(s)
    }

    fn log(base: Instant, list: &[(u64, &str)]) -> Vec<Ev> {
        let mut v = Vec::new();
        for (s, k) in list {
            let t = at(base, *s);
            push(
                &mut v,
                match *k {
                    "start" => Ev::Start(t),
                    "tool" => Ev::ToolStart(t, "query".into()),
                    "end" => Ev::ToolEnd(t),
                    "text" => Ev::Text(t),
                    "beat" => Ev::Heartbeat(t),
                    "done" => Ev::Done(t),
                    _ => Ev::Cancel(t),
                },
            );
        }
        v
    }

    #[test]
    fn phases_follow_the_events_in_order() {
        let b = Instant::now();
        let v = log(b, &[(0, "start")]);
        assert_eq!(stream_status(&v, at(b, 1)).phase, Phase::Waiting);
        let v = log(b, &[(0, "start"), (1, "tool")]);
        let r = stream_status(&v, at(b, 2));
        assert_eq!((r.phase, r.tool.as_deref()), (Phase::Reading, Some("query")));
        let v = log(b, &[(0, "start"), (1, "tool"), (2, "end")]);
        assert_eq!(stream_status(&v, at(b, 3)).phase, Phase::Working);
        let v = log(b, &[(0, "start"), (1, "tool"), (2, "end"), (3, "text")]);
        let r = stream_status(&v, at(b, 4));
        assert_eq!(r.phase, Phase::Answering);
        assert!(r.caret);
        let v = log(b, &[(0, "start"), (3, "text"), (4, "done")]);
        assert_eq!(stream_status(&v, at(b, 9)).phase, Phase::Idle);
    }

    #[test]
    fn counts_tools_and_seconds() {
        let b = Instant::now();
        let v = log(
            b,
            &[(0, "start"), (1, "tool"), (2, "end"), (3, "tool"), (4, "end"), (5, "tool")],
        );
        let r = stream_status(&v, at(b, 12));
        assert_eq!((r.tools, r.secs), (3, 12));
    }

    #[test]
    fn stalls_after_twenty_seconds_without_data() {
        let b = Instant::now();
        let v = log(b, &[(0, "start"), (2, "text")]);
        assert!(!stream_status(&v, at(b, 21)).stalled);
        assert!(stream_status(&v, at(b, 22)).stalled);
    }

    #[test]
    fn heartbeat_resets_the_stall_timer() {
        let b = Instant::now();
        let v = log(b, &[(0, "start"), (15, "beat")]);
        assert!(!stream_status(&v, at(b, 34)).stalled);
        assert!(stream_status(&v, at(b, 35)).stalled);
        // a heartbeat after text keeps the answer phase
        let v = log(b, &[(0, "start"), (1, "text"), (5, "beat")]);
        assert_eq!(stream_status(&v, at(b, 6)).phase, Phase::Answering);
    }

    #[test]
    fn cancel_ends_at_once_and_marks_incomplete() {
        let b = Instant::now();
        let v = log(b, &[(0, "start"), (1, "tool"), (3, "cancel")]);
        let r = stream_status(&v, at(b, 90));
        assert_eq!(r.phase, Phase::Cancelled);
        assert!(r.incomplete && !r.stalled && !r.caret && !r.live());
        assert_eq!(r.secs, 3);
        assert_eq!(repaint_after(&r), None);
    }

    #[test]
    fn no_repaint_when_idle_and_a_tick_while_streaming() {
        let b = Instant::now();
        assert_eq!(repaint_after(&stream_status(&[], b)), None);
        let v = log(b, &[(0, "start"), (1, "done")]);
        assert_eq!(repaint_after(&stream_status(&v, at(b, 5))), None);
        let v = log(b, &[(0, "start")]);
        assert_eq!(repaint_after(&stream_status(&v, at(b, 1))), Some(TICK));
    }

    #[test]
    fn a_new_turn_forgets_the_old_one_and_text_folds() {
        let b = Instant::now();
        let mut v = log(b, &[(0, "start"), (1, "text"), (2, "text"), (3, "beat"), (4, "text")]);
        assert_eq!(v.len(), 2);
        push(&mut v, Ev::Start(at(b, 10)));
        assert_eq!(v.len(), 1);
    }

    fn line(lang: Lang, row: &StatusRow) -> String {
        row_text(row, &|k, a| crate::tr_in(lang, k, a))
    }

    #[test]
    fn row_text_reads_in_both_languages() {
        let b = Instant::now();
        let v = log(b, &[(0, "start"), (1, "tool"), (2, "end"), (3, "tool"), (4, "end"), (5, "tool")]);
        let r = stream_status(&v, at(b, 12));
        assert_eq!(line(Lang::En, &r), "Reading the model \u{b7} 3 tools \u{b7} 12 s");
        let ru = line(Lang::Ru, &r);
        assert!(ru.contains("12") && ru.contains('3') && ru != line(Lang::En, &r));
        let v = log(b, &[(0, "start")]);
        let st = stream_status(&v, at(b, 30));
        assert_eq!(line(Lang::En, &st), "The model service has not replied for 30 s \u{b7} 30 s");
        // a tool that runs for long is work on this computer, not a slow service
        let v = log(b, &[(0, "start"), (1, "tool")]);
        let st = stream_status(&v, at(b, 61));
        assert!(!st.stalled && st.local_busy);
        assert!(line(Lang::En, &st).starts_with("Working on this computer"));
        assert!(line(Lang::En, &st).ends_with("61 s"));
        assert_ne!(line(Lang::Ru, &st), line(Lang::En, &st));
    }

    #[test]
    fn written_tokens_give_a_speed_and_a_tool_event_closes_the_stretch() {
        let b = Instant::now();
        let mut v = log(b, &[(0, "start")]);
        push(&mut v, Ev::Tokens { first: at(b, 1), last: at(b, 1), n: 100 });
        push(&mut v, Ev::Tokens { first: at(b, 5), last: at(b, 5), n: 300 });
        let r = stream_status(&v, at(b, 6));
        assert_eq!((r.written, r.rate, r.phase), (400, Some(100), Phase::Answering));
        assert_eq!(v.iter().filter(|e| matches!(e, Ev::Tokens { .. })).count(), 1);
        assert!(line(Lang::En, &r).starts_with("The model is writing, 400 tokens \u{b7} 100 tok/s"));
        push(&mut v, Ev::ToolStart(at(b, 41), "query".into()));
        assert_eq!(stream_status(&v, at(b, 42)).written, 0);
    }

    #[test]
    fn building_the_draft_here_is_told_with_its_progress() {
        let b = Instant::now();
        let mut v = log(b, &[(0, "start"), (1, "tool")]);
        push(&mut v, Ev::Build(at(b, 2), 120, 3000));
        push(&mut v, Ev::Build(at(b, 3), 240, 3000));
        let r = stream_status(&v, at(b, 4));
        assert_eq!(r.build, Some((240, 3000)));
        assert_eq!(line(Lang::En, &r), "Building the draft here: 240 of 3000 \u{b7} 4 s");
    }

    #[test]
    fn every_key_exists_in_en_and_ru() {
        for k in [
            "ai.stream.waiting",
            "ai.stream.reading",
            "ai.stream.working",
            "ai.stream.answering",
            "ai.stream.tools_one",
            "ai.stream.tools_many",
            "ai.stream.secs",
            "ai.stream.sep",
            "ai.stream.stalled",
            "ai.stream.local",
            "ai.stream.stop",
            "ai.stream.cancelled",
            "ai.stream.incomplete",
            "ai.stream.writing",
            "ai.stream.rate",
            "ai.stream.building",
        ] {
            for l in [Lang::En, Lang::Ru] {
                assert_ne!(crate::tr_in(l, k, &[]), k, "{k}");
            }
        }
    }
}
