//! Values the assistant filled in that no person has verified yet (`value_src`).

use std::collections::BTreeMap;

use super::util::{key, Sink, Within};
use super::{CheckRule, CheckScope, Finding, Fix, Measure, Severity};
use crate::{Error, Model};

/// Elements listed per finding; the count says how many there are.
const SHOWN: usize = 50;

pub(super) struct AiUnverified;

impl CheckRule for AiUnverified {
    fn id(&self) -> &'static str {
        "ai_unverified"
    }

    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let w = Within::new(scope);
        // one finding per parameter: hundreds of equal lines would add nothing
        let mut by_param: BTreeMap<String, Vec<(String, i64)>> = BTreeMap::new();
        for ((tbl, id, name), _) in m.unverified_ai()? {
            if w.has(&tbl, id) && w.in_level(m, &tbl, id) {
                by_param.entry(name).or_default().push((tbl, id));
            }
        }
        let mut s = Sink::new(out, self.id());
        for (name, cells) in by_param {
            let n = cells.len();
            let elements = cells.iter().take(SHOWN).map(|(t, id)| key(t, *id)).collect();
            s.add(
                Severity::Info,
                elements,
                None,
                "ai.check.ai_unverified",
                vec![("param", name), ("count", n.to_string())],
                Fix::None,
                Measure::count(n, 0),
            );
        }
        s.done();
        Ok(())
    }
}
