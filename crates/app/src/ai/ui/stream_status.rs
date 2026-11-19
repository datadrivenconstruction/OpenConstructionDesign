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
            | Ev::Cancel(t) => *t,
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
    /// The turn was stopped before it finished: any draft is partial and nothing was applied.
    pub incomplete: bool,
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
            incomplete: false,
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
        row.stalled = now.saturating_duration_since(last) >= STALL_AFTER;
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

/// The row as one line: "Reading the model - 3 tools - 12 s".
pub fn row_text(row: &StatusRow, tr: &dyn Fn(&str, &[(&str, &str)]) -> String) -> String {
    if row.stalled {
        return tr("ai.stream.stalled", &[]);
    }
    let phase = match row.phase {
        Phase::Idle => return String::new(),
        Phase::Cancelled => return tr("ai.stream.cancelled", &[]),
        Phase::Waiting => "ai.stream.waiting",
        Phase::Reading => "ai.stream.reading",
        Phase::Working => "ai.stream.working",
        Phase::Answering => "ai.stream.answering",
    };
    let mut parts = vec![tr(phase, &[])];
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
        let v = log(b, &[(0, "start"), (2, "tool")]);
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
        assert_eq!(line(Lang::En, &st), "Still waiting for the model service");
        assert_ne!(line(Lang::Ru, &st), line(Lang::En, &st));
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
            "ai.stream.stop",
            "ai.stream.cancelled",
            "ai.stream.incomplete",
        ] {
            for l in [Lang::En, Lang::Ru] {
                assert_ne!(crate::tr_in(l, k, &[]), k, "{k}");
            }
        }
    }
}
