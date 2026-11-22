// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Oracle for whether every element shows a material look: `materials` passes when no element
//! of the checked tables (`material_look::CHECKED`) has an empty material or a name that
//! resolves to no library look, so nothing renders as plain default grey.

use super::case::GeomO;
use super::oracle::{result, EvalCx, Oracle, OracleResult};

pub fn build(g: &GeomO) -> Option<Box<dyn Oracle>> {
    (g.kind == "materials").then(|| Box::new(Looks) as Box<dyn Oracle>)
}

struct Looks;

impl Oracle for Looks {
    fn name(&self) -> String {
        "geom materials".into()
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let name = self.name();
        let want = "every element has a material with a look".to_string();
        match cx.ed.model.material_gaps() {
            Ok(g) if g.is_empty() => result(&name, want, "0 without a look".into(), true),
            Ok(g) => {
                let first: Vec<String> =
                    g.iter().take(5).map(|x| format!("{}:{} '{}'", x.table, x.id, x.material)).collect();
                result(&name, want, format!("{} without a look: {}", g.len(), first.join(", ")), false)
            }
            Err(e) => result(&name, want, e.to_string(), false),
        }
    }
}
