// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Finishes schedule: per room the floor, wall and ceiling finish and the areas they cover.
//! A finish comes from the room's own parameters (`FinishFloor`, `FinishWalls`,
//! `FinishCeiling`) when the project has them, else from a norm-pack assembly with
//! `"role": "finish"` naming the room kind, else from the room-kind rules of
//! `data/finishes.json`. The result carries which of the three decided.

use std::sync::OnceLock;

use serde_json::Value as J;

use crate::area_std::class_of;
use crate::checks::norm_pack::NormPack;
use crate::{Error, Model, PVal, ParamType};

/// Room parameters that set a finish by hand: (name, type, category).
pub const FINISH_PARAMS: [(&str, ParamType, &str); 3] = [
    ("FinishFloor", ParamType::Text, "room"),
    ("FinishWalls", ParamType::Text, "room"),
    ("FinishCeiling", ParamType::Text, "room"),
];

/// What decided the finishes of a room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinishSource {
    /// The room's own parameters (the ones left empty follow the rules).
    Param,
    /// A finish assembly of the norm pack.
    Pack,
    /// The room-kind rules of the editor.
    Rule,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoomFinish {
    pub room: i64,
    pub level: i64,
    pub level_name: String,
    pub number: String,
    pub name: String,
    /// Finish ids (`tile`, `paint`, ...) or the free text of a parameter.
    pub floor: String,
    pub walls: String,
    pub ceiling: String,
    /// m2: the room's net area.
    pub floor_area: f64,
    /// m2: perimeter times room height, openings not deducted.
    pub wall_area: f64,
    pub source: FinishSource,
}

type Triple = (String, String, String);

fn rules() -> &'static J {
    static R: OnceLock<J> = OnceLock::new();
    R.get_or_init(|| {
        serde_json::from_str(include_str!("../data/finishes.json")).expect("finishes.json is valid")
    })
}

fn triple(v: &J) -> Triple {
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    (s("floor"), s("walls"), s("ceiling"))
}

fn lists(v: &J, key: &str, what: &str) -> bool {
    match &v[key] {
        J::Array(a) => a.iter().any(|x| x.as_str() == Some(what)),
        J::String(s) => s == what,
        _ => false,
    }
}

/// Finishes of a room kind and use class by the rules, and the pack assembly when it has one.
fn rule_for(pack: &NormPack, kind: Option<&str>, class: &str) -> (Triple, FinishSource) {
    if let Some(k) = kind {
        if let Some(a) = pack
            .assemblies
            .iter()
            .find(|a| a["role"] == "finish" && lists(a, "room_kind", k))
        {
            return (triple(a), FinishSource::Pack);
        }
    }
    let rs = rules();
    let by = |key: &str, what: &str| {
        rs["rules"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| lists(r, key, what))
    };
    let hit = kind
        .and_then(|k| by("kinds", k))
        .or_else(|| by("classes", class))
        .unwrap_or(&rs["default"]);
    (triple(hit), FinishSource::Rule)
}

impl Model {
    /// Floor finish of each room of a storey for its tag: by the project's pack, else generic.
    /// Empty when the rules cannot be read; a tag then carries no finish.
    pub(crate) fn plan_room_finishes(&self, level: i64) -> std::collections::BTreeMap<i64, String> {
        let set = crate::checks::norm_pack::PackSet::embedded();
        let stored = self.norm_locale().map(|l| l.pack).unwrap_or_default();
        let id = if set.has(&stored) { stored } else { "generic".to_string() };
        let Ok(pack) = set.resolve(&id) else { return Default::default() };
        self.room_finishes_at(&pack, Some(level))
            .unwrap_or_default()
            .into_iter()
            .filter(|f| !f.floor.is_empty())
            .map(|f| (f.room, f.floor))
            .collect()
    }

    /// Adds the missing finish parameters in one undoable step; returns how many were added.
    pub fn ensure_finish_params(&mut self) -> Result<usize, Error> {
        let mut specs = vec![];
        for (name, ptype, cat) in FINISH_PARAMS {
            if self.param_by_name(name)?.is_none() {
                specs.push(crate::ParamSpec {
                    name: name.to_string(),
                    ptype,
                    per_type: false,
                    unit: String::new(),
                    group: "Finishes".to_string(),
                    guid: None,
                    categories: vec![cat.to_string()],
                });
            }
        }
        if specs.is_empty() {
            return Ok(0);
        }
        Ok(self.add_params(&specs)?.len())
    }

    /// Finish of every room with a closed outline, by storey then room.
    pub fn room_finishes(&self, pack: &NormPack) -> Result<Vec<RoomFinish>, Error> {
        self.room_finishes_at(pack, None)
    }

    /// As `room_finishes`, for one storey when `only` is set.
    pub fn room_finishes_at(
        &self,
        pack: &NormPack,
        only: Option<i64>,
    ) -> Result<Vec<RoomFinish>, Error> {
        let defs: Vec<_> = FINISH_PARAMS
            .iter()
            .map(|(n, ..)| self.param_by_name(n))
            .collect::<Result<_, _>>()?;
        let purpose = self.param_by_name("Purpose")?;
        let text = |d: &Option<crate::ParamDef>, id: i64| -> Result<Option<String>, Error> {
            Ok(match d {
                Some(d) => match self.param_value_of(d, "room", id)? {
                    Some(PVal::Text(t)) if !t.trim().is_empty() => Some(t.trim().to_string()),
                    _ => None,
                },
                None => None,
            })
        };
        let mut out = vec![];
        for (level, level_name, _) in self.level_list()? {
            if only.is_some_and(|o| o != level) {
                continue;
            }
            let mut st = self.db.prepare(
                "SELECT r.id, r.number, r.name, r.height FROM room r JOIN room_qty q ON q.room=r.id \
                 WHERE r.level=?1 ORDER BY r.id",
            )?;
            let rows: Vec<(i64, String, String, f64)> = st
                .query_map([level], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?)))?
                .collect::<Result<_, _>>()?;
            for (id, number, name, height) in rows {
                let Some(q) = self.room_quantities(id)? else { continue };
                let p = text(&purpose, id)?;
                let kind = p
                    .as_deref()
                    .and_then(|p| pack.purpose.get(p).map(String::as_str))
                    .or_else(|| pack.kind_of_name(&name));
                let class = class_of(pack, p.as_deref(), &name);
                let ((f, w, c), mut source) = rule_for(pack, kind, &class);
                let mine = [text(&defs[0], id)?, text(&defs[1], id)?, text(&defs[2], id)?];
                if mine.iter().any(Option::is_some) {
                    source = FinishSource::Param;
                }
                let [mf, mw, mc] = mine;
                out.push(RoomFinish {
                    room: id,
                    level,
                    level_name: level_name.clone(),
                    number,
                    name,
                    floor: mf.unwrap_or(f),
                    walls: mw.unwrap_or(w),
                    ceiling: mc.unwrap_or(c),
                    floor_area: q.area,
                    wall_area: q.perimeter * height,
                    source,
                });
            }
        }
        Ok(out)
    }
}
