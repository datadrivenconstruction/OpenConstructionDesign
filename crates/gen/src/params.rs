//! Bounded parameter reading: every value is checked before a generator sees it.

use crate::{GenError, ParamDef, ParamKind};
use serde_json::Value as J;

pub(crate) fn bad(name: &str, why: impl Into<String>) -> GenError {
    GenError::Param {
        name: name.to_string(),
        why: why.into(),
    }
}

pub(crate) struct Args<'a> {
    defs: &'static [ParamDef],
    raw: &'a J,
}

impl<'a> Args<'a> {
    pub fn new(defs: &'static [ParamDef], raw: &'a J) -> Result<Self, GenError> {
        match raw {
            J::Null => {}
            J::Object(o) => {
                for k in o.keys() {
                    if !defs.iter().any(|d| d.name == k) {
                        return Err(bad(k, "unknown parameter"));
                    }
                }
            }
            _ => return Err(bad("params", "must be an object")),
        }
        for d in defs {
            let v = &raw[d.name];
            if !v.is_null() {
                check(d, v)?;
            }
        }
        Ok(Args { defs, raw })
    }

    fn def(&self, name: &str) -> &'static ParamDef {
        // names are this crate's own constants: a miss is a bug in the generator
        self.defs
            .iter()
            .find(|d| d.name == name)
            .expect("declared parameter")
    }

    pub fn num(&self, name: &str) -> f64 {
        self.raw[name]
            .as_f64()
            .unwrap_or_else(|| self.def(name).default)
    }

    pub fn count(&self, name: &str) -> usize {
        self.num(name).round() as usize
    }

    pub fn choice(&self, name: &str) -> &'static str {
        let d = self.def(name);
        let ParamKind::Choice(list) = d.kind else {
            return "";
        };
        match self.raw[name].as_str() {
            Some(s) => list.iter().find(|c| **c == s).copied().unwrap_or(list[0]),
            None => list[d.default as usize],
        }
    }

    pub fn raw(&self, name: &str) -> &'a J {
        &self.raw[name]
    }

    /// The built-in rule set with a checked `Rules` value applied at the brief layer.
    pub fn rules(&self, name: &str) -> Result<crate::project::rules::RuleSet, GenError> {
        use crate::project::rules::{Layer, RuleSet};
        let mut rs = RuleSet::new();
        for (id, v) in self.raw[name].as_object().into_iter().flatten() {
            let x = crate::project::rules::def(id).and_then(|d| d.parse_value(v)).ok_or_else(|| bad(name, format!("no value for {id}")))?;
            rs.set(Layer::Brief, id, x).map_err(|e| bad(name, e.to_string()))?;
        }
        Ok(rs)
    }

    /// A checked `Points` value; `None` when not given.
    pub fn points(&self, name: &str) -> Option<Vec<(f64, f64)>> {
        let a = self.raw[name].as_array()?;
        Some(a.iter().filter_map(|p| Some((p[0].as_f64()?, p[1].as_f64()?))).collect())
    }
}

fn check(d: &ParamDef, v: &J) -> Result<(), GenError> {
    match d.kind {
        ParamKind::Length | ParamKind::Area | ParamKind::Ratio | ParamKind::Count => {
            let x = v
                .as_f64()
                .filter(|x| x.is_finite())
                .ok_or_else(|| bad(d.name, "must be a number"))?;
            if x < d.min || x > d.max {
                return Err(bad(d.name, format!("must be within {}..{}", d.min, d.max)));
            }
            if d.kind == ParamKind::Count && x.fract() != 0.0 {
                return Err(bad(d.name, "must be a whole number"));
            }
        }
        ParamKind::Choice(list) => {
            let s = v.as_str().ok_or_else(|| bad(d.name, "must be text"))?;
            if !list.contains(&s) {
                return Err(bad(d.name, format!("must be one of {}", list.join(", "))));
            }
        }
        ParamKind::RoomList => {
            if !v.is_array() {
                return Err(bad(d.name, "must be a list"));
            }
        }
        ParamKind::Points => {
            let ok = v.as_array().is_some_and(|a| {
                (a.len() as f64) >= d.min
                    && (a.len() as f64) <= d.max
                    && a.iter().all(|p| p.as_array().is_some_and(|q| q.len() == 2 && q.iter().all(|x| x.as_f64().is_some_and(f64::is_finite))))
            });
            if !ok {
                return Err(bad(d.name, format!("must be a list of {}..{} [x, y] number pairs", d.min, d.max)));
            }
        }
        ParamKind::Rules => {
            let o = v.as_object().ok_or_else(|| bad(d.name, "must be an object of rule id -> value"))?;
            for (id, x) in o {
                let r = crate::project::rules::def(id).ok_or_else(|| bad(d.name, format!("no rule {id}")))?;
                if !r.parse_value(x).is_some_and(|x| r.contains(x)) {
                    return Err(bad(d.name, format!("{id} must be {}", r.takes())));
                }
            }
        }
    }
    Ok(())
}
