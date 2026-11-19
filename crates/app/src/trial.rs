//! Ghost preview before commit for ordinary tools. While a tool holds a valid input, the edit it
//! would make runs as a model trial (`Model::trial`, always rolled back) once the input has been
//! still for `SETTLE` seconds. The result is a ghost of the changed walls and slabs, or a refusal
//! attached to the input field that caused it; a refusal blocks the commit. Newer input drops the
//! stale result, and a ghost is shown only for the document and frame it was asked for.
//! A trial slower than `BUDGET` turns trials off for the rest of the tool session (the tool keeps
//! its plain outline); a smaller selection or a new tool turns them on again.

use crate::editor::OpFn;
use ddc_geom::{v2, V2};
use ddc_model::Model;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use web_time::Instant;

/// Seconds the input must stay unchanged before a trial runs.
pub const SETTLE: f64 = 0.25;

/// Seconds one trial may take; a slower one turns trials off for the tool session.
pub const BUDGET: f64 = 0.030;

/// Tool input that a refusal is shown under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    /// The picked point (move/copy target point).
    Point,
    /// A typed or set length (offset distance).
    Distance,
    /// The element under the cursor.
    Target,
}

impl Field {
    pub fn label_key(self) -> &'static str {
        match self {
            Field::Point => "trial.field.point",
            Field::Distance => "trial.field.distance",
            Field::Target => "trial.field.target",
        }
    }
}

/// The edit a tool would commit now, with a key of the input that defines it.
#[derive(Clone, Debug)]
pub struct Ask {
    pub key: u64,
    pub field: Field,
    pub op: OpFn,
    /// Elements the edit touches; fewer than when trials went off turns them on again.
    pub size: usize,
}

/// Builds an input key from its parts; lengths are compared to 1e-6 m.
#[derive(Default)]
pub struct Key(std::collections::hash_map::DefaultHasher);

impl Key {
    pub fn new(tag: &str) -> Self {
        let mut k = Key::default();
        tag.hash(&mut k.0);
        k
    }
    pub fn f(mut self, x: f64) -> Self {
        ((x * 1e6).round() as i64).hash(&mut self.0);
        self
    }
    pub fn p(self, p: V2) -> Self {
        self.f(p.x).f(p.y)
    }
    pub fn h(mut self, x: impl Hash) -> Self {
        x.hash(&mut self.0);
        self
    }
    pub fn done(self) -> u64 {
        self.0.finish()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// Changed and new walls (two points) and slab outlines (closed rings) on the level.
    Ghost(Vec<Vec<V2>>),
    Refused { field: Field, reason: String },
}

#[derive(Clone, Debug)]
struct Done {
    key: u64,
    doc: u64,
    verdict: Verdict,
}

#[derive(Clone, Debug, Default)]
pub struct Trial {
    /// Current input key and the time it last changed.
    input: Option<(u64, f64)>,
    done: Option<Done>,
    /// Frame of the last request (`pulse`).
    asked: Option<u64>,
    /// Trials run so far.
    pub runs: u64,
    /// Set by a trial over `BUDGET`: the `Ask::size` it had. No trials run while set.
    off: Option<usize>,
    /// Seconds allowed on top of `BUDGET`; 0 in the app. Result tests on a loaded machine
    /// raise it so a slow run does not turn the ghost off.
    pub(crate) slack: f64,
}

impl Trial {
    /// Once per frame with what the tool would commit now. Runs the trial when the input has
    /// been still for `SETTLE`; true when it ran.
    pub fn pulse(
        &mut self,
        model: &mut Model,
        level: i64,
        ask: Option<&Ask>,
        now: f64,
        frame: u64,
    ) -> bool {
        self.asked = Some(frame);
        let Some(ask) = ask else {
            self.input = None;
            self.done = None;
            return false;
        };
        if self.off.is_some_and(|n| ask.size < n) {
            self.off = None;
        }
        if self.input.map(|(k, _)| k) != Some(ask.key) {
            // newer input: the stale result goes, the wait starts again
            self.input = Some((ask.key, now));
            if self.done.as_ref().is_some_and(|d| d.key != ask.key) {
                self.done = None;
            }
            return false;
        }
        let doc = model.serial();
        if self.done.as_ref().is_some_and(|d| d.key == ask.key && d.doc == doc) {
            return false;
        }
        let since = self.input.map_or(now, |(_, t)| t);
        if now - since < SETTLE || self.off.is_some() {
            return false;
        }
        let t0 = Instant::now();
        let verdict = run(model, level, ask);
        if t0.elapsed().as_secs_f64() > BUDGET + self.slack {
            self.off = Some(ask.size);
        }
        // kept even when over budget: its refusal still blocks the commit of this input
        self.done = Some(Done {
            key: ask.key,
            doc,
            verdict,
        });
        self.runs += 1;
        true
    }

    /// Trials are off for this tool session (the last one was over `BUDGET`).
    pub fn is_off(&self) -> bool {
        self.off.is_some()
    }

    /// Muted hint shown in the tool bar while trials are off.
    pub fn off_hint(&self) -> Option<String> {
        self.is_off().then(|| crate::tr("trial.preview_off", &[]))
    }

    /// Seconds until the pending trial is due (for a repaint), if one is waiting.
    pub fn wait(&self, now: f64) -> Option<f64> {
        let (k, t) = self.input?;
        if self.off.is_some() || self.done.as_ref().is_some_and(|d| d.key == k) {
            return None;
        }
        Some((t + SETTLE - now).max(0.0))
    }

    fn current(&self, doc: u64) -> Option<&Done> {
        let (k, _) = self.input?;
        self.done
            .as_ref()
            .filter(|d| d.key == k && d.doc == doc)
    }

    /// The verdict for `key` in document `doc`, if a trial of exactly that input finished.
    pub fn verdict(&self, key: u64, doc: u64) -> Option<&Verdict> {
        self.done
            .as_ref()
            .filter(|d| d.key == key && d.doc == doc)
            .map(|d| &d.verdict)
    }

    /// The refusal of the current input, for the field it belongs to.
    pub fn refusal(&self, doc: u64) -> Option<(Field, &str)> {
        match &self.current(doc)?.verdict {
            Verdict::Refused { field, reason } => Some((*field, reason.as_str())),
            Verdict::Ghost(_) => None,
        }
    }

    /// Commit gate: the refusal found for exactly this input blocks it.
    pub fn blocks(&self, key: u64, doc: u64) -> Option<(Field, String)> {
        match self.verdict(key, doc)? {
            Verdict::Refused { field, reason } => Some((*field, reason.clone())),
            Verdict::Ghost(_) => None,
        }
    }

    /// The ghost to draw: only for this document and only if asked this frame or the last.
    pub fn ghost(&self, doc: u64, frame: u64) -> Option<&[Vec<V2>]> {
        if self.off.is_some() || self.asked? + 1 < frame {
            return None;
        }
        match &self.current(doc)?.verdict {
            Verdict::Ghost(g) => Some(g.as_slice()),
            Verdict::Refused { .. } => None,
        }
    }

    pub fn clear(&mut self) {
        self.input = None;
        self.done = None;
    }
}

type Shapes = HashMap<(u8, i64), Vec<V2>>;

/// Walls and slab outer rings on the level, by element.
fn shapes(m: &Model, level: i64) -> Result<Shapes, ddc_model::Error> {
    let db = m.sql();
    let mut out: Shapes = HashMap::new();
    let mut st = db.prepare("SELECT id, ax, ay, bx, by FROM wall WHERE level=?1")?;
    let rows = st.query_map([level], |r| {
        Ok((r.get::<_, i64>(0)?, v2(r.get(1)?, r.get(2)?), v2(r.get(3)?, r.get(4)?)))
    })?;
    for r in rows {
        let (id, a, b) = r?;
        out.insert((0, id), vec![a, b]);
    }
    let mut st = db.prepare(
        "SELECT v.slab, v.x, v.y FROM slab_vertex v JOIN slab s ON s.id=v.slab \
         WHERE s.level=?1 AND v.ring=0 ORDER BY v.slab, v.idx",
    )?;
    let rows = st.query_map([level], |r| {
        Ok((r.get::<_, i64>(0)?, v2(r.get(1)?, r.get(2)?)))
    })?;
    for r in rows {
        let (id, p) = r?;
        out.entry((1, id)).or_default().push(p);
    }
    Ok(out)
}

/// One trial of `ask` on the model: the shapes that changed or appeared, or the refusal.
pub fn run(model: &mut Model, level: i64, ask: &Ask) -> Verdict {
    let r = model.trial(|m| {
        let before = shapes(m, level)?;
        (ask.op.0)(m)?;
        let after = shapes(m, level)?;
        let mut keys: Vec<&(u8, i64)> = after
            .iter()
            .filter(|(k, s)| before.get(*k) != Some(*s))
            .map(|(k, _)| k)
            .collect();
        keys.sort();
        Ok(keys
            .into_iter()
            .map(|k| {
                let mut s = after[k].clone();
                if k.0 == 1 && s.len() > 2 {
                    s.push(s[0]);
                }
                s
            })
            .collect::<Vec<_>>())
    });
    match r {
        Ok(g) => Verdict::Ghost(g),
        Err(e) => Verdict::Refused {
            field: ask.field,
            reason: crate::elem::mep::explain(&e),
        },
    }
}

/// The refusal line shown under a field: "<field>: <reason>".
pub fn refusal_text(field: Field, reason: &str) -> String {
    crate::tr(
        "trial.refused",
        &[("field", &crate::tr(field.label_key(), &[])), ("reason", &reason.to_string())],
    )
}

/// Ghost polylines over the plan.
pub fn paint(
    p: &egui::Painter,
    ghost: &[Vec<V2>],
    to_s: &dyn Fn(V2) -> egui::Pos2,
    st: egui::Stroke,
) {
    for s in ghost {
        for w in s.windows(2) {
            crate::overlay::ghost_line(p, to_s(w[0]), to_s(w[1]), st);
        }
    }
}

#[cfg(test)]
#[path = "trial_tests.rs"]
mod tests;
